#[derive(Clone)]
struct AppState {
    default_workdir: String,
    persistence_path: Arc<PathBuf>,
    orchestrator_templates_path: Arc<PathBuf>,
    state_events: broadcast::Sender<String>,
    delta_events: broadcast::Sender<String>,
    shared_codex_runtime: Arc<Mutex<Option<SharedCodexRuntime>>>,
    remote_registry: Arc<RemoteRegistry>,
    inner: Arc<Mutex<StateInner>>,
}

impl AppState {
    fn new(default_workdir: String) -> Result<Self> {
        let persistence_path = resolve_persistence_path(&default_workdir);
        let orchestrator_templates_path = resolve_orchestrator_templates_path(&default_workdir);
        let mut inner = load_state(&persistence_path)?.unwrap_or_else(|| {
            let mut inner = StateInner::new();
            let default_project =
                inner.create_project(None, default_workdir.clone(), default_local_remote_id());
            inner.create_session(
                Agent::Codex,
                Some("Codex Live".to_owned()),
                default_workdir.clone(),
                Some(default_project.id.clone()),
                None,
            );
            inner.create_session(
                Agent::Claude,
                Some("Claude Live".to_owned()),
                default_workdir.clone(),
                Some(default_project.id.clone()),
                None,
            );
            inner
        });
        let discovery_scopes = collect_codex_discovery_scopes(&default_workdir, &inner.projects);
        match discover_codex_threads(&default_workdir, &discovery_scopes) {
            Ok(discovered_threads) => {
                inner.import_discovered_codex_threads(&default_workdir, discovered_threads);
            }
            Err(err) => {
                eprintln!("codex discovery> failed to load Codex thread metadata: {err:#}");
            }
        }

        let state = Self {
            default_workdir,
            persistence_path: Arc::new(persistence_path),
            orchestrator_templates_path: Arc::new(orchestrator_templates_path),
            state_events: broadcast::channel(128).0,
            delta_events: broadcast::channel(256).0,
            shared_codex_runtime: Arc::new(Mutex::new(None)),
            remote_registry: Arc::new(
                std::thread::spawn(RemoteRegistry::new)
                    .join()
                    .expect("remote registry init thread panicked")?,
            ),
            inner: Arc::new(Mutex::new(inner)),
        };
        state.seed_hidden_claude_spares();
        {
            let inner = state.inner.lock().expect("state mutex poisoned");
            state.persist_internal_locked(&inner)?;
        }
        state.restore_remote_event_bridges();
        Ok(state)
    }

    fn snapshot(&self) -> StateResponse {
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.snapshot_from_inner(&inner)
    }

    fn list_agent_commands(
        &self,
        session_id: &str,
    ) -> std::result::Result<AgentCommandsResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_list_agent_commands(session_id);
        }

        let (session, cached_agent_commands) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            (
                inner.sessions[index].session.clone(),
                inner.sessions[index].agent_commands.clone(),
            )
        };

        let filesystem_commands = read_claude_agent_commands(FsPath::new(&session.workdir))?;
        let commands = if session.agent == Agent::Claude {
            merge_agent_commands(&cached_agent_commands, &filesystem_commands)
        } else {
            filesystem_commands
        };

        Ok(AgentCommandsResponse { commands })
    }

    fn search_instructions(
        &self,
        session_id: &str,
        query: &str,
    ) -> std::result::Result<InstructionSearchResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_search_instructions(session_id, query);
        }

        let session = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner.sessions[index].session.clone()
        };

        search_instruction_phrase(FsPath::new(&session.workdir), query)
    }

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
                .unwrap_or_else(default_claude_approval_mode);
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
                let record = &mut inner.sessions[index];
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
        if let Some(slot) = inner
            .find_session_index(&record.session.id)
            .and_then(|index| inner.sessions.get_mut(index))
        {
            *slot = record.clone();
        }
        self.commit_locked(&mut inner)
            .map_err(|err| ApiError::internal(format!("failed to persist session: {err:#}")))?;
        drop(inner);
        if let Some(session_id) = hidden_claude_spare_to_spawn {
            self.try_start_hidden_claude_spare(&session_id);
        }
        Ok(CreateSessionResponse {
            session_id: record.session.id,
            state: {
                let inner = self.inner.lock().expect("state mutex poisoned");
                self.snapshot_from_inner(&inner)
            },
        })
    }

    fn update_app_settings(
        &self,
        request: UpdateAppSettingsRequest,
    ) -> Result<StateResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let mut changed = false;

        if let Some(default_codex_reasoning_effort) = request.default_codex_reasoning_effort {
            if inner.preferences.default_codex_reasoning_effort != default_codex_reasoning_effort {
                inner.preferences.default_codex_reasoning_effort = default_codex_reasoning_effort;
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
        if let Some(remotes) = request.remotes {
            let normalized_remotes = normalize_remote_configs(remotes)?;
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
            self.remote_registry.reconcile(&remotes);
        }
        Ok(snapshot)
    }

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

    fn commit_locked(&self, inner: &mut StateInner) -> Result<u64> {
        let revision = self.bump_revision_and_persist_locked(inner)?;
        self.publish_state_locked(inner)?;
        Ok(revision)
    }

    // Internal bookkeeping changes should be persisted without advancing the client-visible revision.
    fn persist_internal_locked(&self, inner: &StateInner) -> Result<()> {
        persist_state(self.persistence_path.as_path(), inner)
    }

    // Delta-producing changes advance the revision without publishing a full snapshot; the delta event
    // carries the new revision instead. Persisting the full state on every streamed chunk makes
    // long responses increasingly slow, so durable persistence is deferred until the next
    // non-delta commit.
    fn commit_delta_locked(&self, inner: &mut StateInner) -> Result<u64> {
        inner.revision += 1;
        Ok(inner.revision)
    }

    // Some live-update paths still need durable persistence, but should not force a full-state
    // SSE snapshot when a small targeted delta is enough for the UI.
    fn commit_persisted_delta_locked(&self, inner: &mut StateInner) -> Result<u64> {
        self.bump_revision_and_persist_locked(inner)
    }

    fn bump_revision_and_persist_locked(&self, inner: &mut StateInner) -> Result<u64> {
        inner.revision += 1;
        self.persist_internal_locked(inner)?;
        Ok(inner.revision)
    }

    fn subscribe_events(&self) -> broadcast::Receiver<String> {
        self.state_events.subscribe()
    }

    fn subscribe_delta_events(&self) -> broadcast::Receiver<String> {
        self.delta_events.subscribe()
    }

    fn publish_delta(&self, event: &DeltaEvent) {
        if let Ok(payload) = serde_json::to_string(event) {
            let _ = self.delta_events.send(payload);
        }
    }

    fn publish_state_locked(&self, inner: &StateInner) -> Result<()> {
        let payload = serde_json::to_string(&self.snapshot_from_inner(inner))
            .context("failed to serialize session snapshot")?;
        let _ = self.state_events.send(payload);
        Ok(())
    }

    fn shared_codex_runtime(&self) -> Result<SharedCodexRuntime> {
        let mut shared_runtime = self
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        if let Some(runtime) = shared_runtime.clone() {
            return Ok(runtime);
        }

        let runtime = spawn_shared_codex_runtime(self.clone())?;
        *shared_runtime = Some(runtime.clone());
        Ok(runtime)
    }

    fn perform_codex_json_rpc_request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ApiError> {
        let runtime = self.shared_codex_runtime().map_err(|err| {
            ApiError::internal(format!("failed to start shared Codex runtime: {err:#}"))
        })?;
        let (response_tx, response_rx) = mpsc::channel::<std::result::Result<Value, String>>();
        runtime
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcRequest {
                method: method.to_owned(),
                params,
                timeout,
                response_tx,
            })
            .map_err(|err| {
                ApiError::internal(format!("failed to queue Codex request `{method}`: {err}"))
            })?;

        match response_rx.recv_timeout(timeout + Duration::from_secs(1)) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(detail)) => Err(ApiError::bad_request(format!(
                "Codex request `{method}` failed: {detail}"
            ))),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ApiError::internal(format!(
                "timed out waiting for Codex request `{method}`"
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ApiError::internal(format!(
                "Codex request `{method}` did not return a result"
            ))),
        }
    }

    fn resolve_codex_thread_action_context(
        &self,
        session_id: &str,
    ) -> Result<CodexThreadActionContext, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &inner.sessions[index];

        if record.session.agent != Agent::Codex {
            return Err(ApiError::bad_request(
                "Codex thread actions are only available for Codex sessions",
            ));
        }
        if matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval
        ) {
            return Err(ApiError::conflict(
                "wait for the current Codex turn to finish before using thread actions",
            ));
        }
        if !record.queued_prompts.is_empty() {
            return Err(ApiError::conflict(
                "wait for queued Codex prompts to finish before using thread actions",
            ));
        }

        let thread_id = record.external_session_id.clone().ok_or_else(|| {
            ApiError::bad_request(
                "Codex thread actions are only available after the session has started a thread",
            )
        })?;

        Ok(CodexThreadActionContext {
            approval_policy: record
                .session
                .approval_policy
                .unwrap_or(record.codex_approval_policy),
            model: record.session.model.clone(),
            model_options: record.session.model_options.clone(),
            name: record.session.name.clone(),
            project_id: record.session.project_id.clone(),
            reasoning_effort: record
                .session
                .reasoning_effort
                .unwrap_or(record.codex_reasoning_effort),
            sandbox_mode: record
                .session
                .sandbox_mode
                .unwrap_or(record.codex_sandbox_mode),
            thread_id,
            thread_state: normalized_codex_thread_state(
                record.session.agent,
                record.external_session_id.as_deref(),
                record.session.codex_thread_state,
            ),
            workdir: record.session.workdir.clone(),
        })
    }

    fn clear_shared_codex_runtime_if_matches(&self, runtime_id: &str) {
        let mut shared_runtime = self
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        if shared_runtime
            .as_ref()
            .is_some_and(|runtime| runtime.runtime_id == runtime_id)
        {
            *shared_runtime = None;
        }
    }

    fn handle_shared_codex_runtime_exit(
        &self,
        runtime_id: &str,
        error_message: Option<&str>,
    ) -> Result<()> {
        let session_ids = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter_map(|record| match &record.runtime {
                    SessionRuntime::Codex(handle) if handle.runtime_id == runtime_id => {
                        Some(record.session.id.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        let token = RuntimeToken::Codex(runtime_id.to_owned());
        for session_id in session_ids {
            self.handle_runtime_exit_if_matches(&session_id, &token, error_message)?;
        }
        self.clear_shared_codex_runtime_if_matches(runtime_id);
        Ok(())
    }

    fn snapshot_from_inner(&self, inner: &StateInner) -> StateResponse {
        StateResponse {
            revision: inner.revision,
            codex: inner.codex.clone(),
            agent_readiness: collect_agent_readiness(&self.default_workdir),
            preferences: inner.preferences.clone(),
            projects: inner.projects.clone(),
            sessions: inner
                .sessions
                .iter()
                .filter(|record| !record.hidden)
                .map(|record| record.session.clone())
                .collect(),
        }
    }

    fn seed_hidden_claude_spares(&self) {
        let spare_ids = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let contexts = inner
                .sessions
                .iter()
                .filter(|record| {
                    !record.hidden
                        && !record.is_remote_proxy()
                        && record.session.agent == Agent::Claude
                })
                .map(|record| {
                    (
                        record.session.workdir.clone(),
                        record.session.project_id.clone(),
                        record.session.model.clone(),
                        record
                            .session
                            .claude_approval_mode
                            .unwrap_or_else(default_claude_approval_mode),
                        record
                            .session
                            .claude_effort
                            .unwrap_or_else(default_claude_effort),
                    )
                })
                .collect::<Vec<_>>();
            let mut spare_ids = Vec::new();
            for (workdir, project_id, model, approval_mode, effort) in contexts {
                if let Some(session_id) = inner.ensure_hidden_claude_spare(
                    workdir,
                    project_id,
                    model,
                    approval_mode,
                    effort,
                ) {
                    spare_ids.push(session_id);
                }
            }
            spare_ids
        };

        for session_id in spare_ids {
            self.try_start_hidden_claude_spare(&session_id);
        }
    }

    fn try_start_hidden_claude_spare(&self, session_id: &str) {
        let spawn_request = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = &mut inner.sessions[index];
            if !record.hidden
                || record.is_remote_proxy()
                || record.session.agent != Agent::Claude
                || !matches!(record.runtime, SessionRuntime::None)
            {
                return;
            }

            reset_hidden_claude_spare_record(record);
            Some((
                record.session.id.clone(),
                record.session.workdir.clone(),
                record.session.model.clone(),
                record
                    .session
                    .claude_approval_mode
                    .unwrap_or_else(default_claude_approval_mode),
                record
                    .session
                    .claude_effort
                    .unwrap_or_else(default_claude_effort),
                record.external_session_id.clone(),
            ))
        };

        let Some((session_id, cwd, model, approval_mode, effort, resume_session_id)) =
            spawn_request
        else {
            return;
        };

        let handle = match spawn_claude_runtime(
            self.clone(),
            session_id.clone(),
            cwd,
            model,
            approval_mode,
            effort,
            resume_session_id,
            None,
        ) {
            Ok(handle) => handle,
            Err(err) => {
                eprintln!("claude hidden pool> failed to warm spare `{session_id}`: {err:#}");
                return;
            }
        };

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(&session_id) else {
            let _ = handle.kill();
            return;
        };
        let record = &mut inner.sessions[index];
        if record.session.agent != Agent::Claude || !matches!(record.runtime, SessionRuntime::None) {
            let _ = handle.kill();
            return;
        }
        record.runtime = SessionRuntime::Claude(handle);
    }

    fn start_turn_on_record(
        &self,
        record: &mut SessionRecord,
        message_id: String,
        prompt: String,
        attachments: Vec<PromptImageAttachment>,
        expanded_prompt: Option<String>,
    ) -> std::result::Result<TurnDispatch, ApiError> {
        let message_attachments = attachments
            .iter()
            .map(|attachment| attachment.metadata.clone())
            .collect::<Vec<_>>();
        let expanded_prompt = expanded_prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != prompt)
            .map(str::to_owned);
        let runtime_prompt = expanded_prompt.clone().unwrap_or_else(|| prompt.clone());

        let dispatch = match record.session.agent {
            Agent::Claude => {
                if record.runtime_reset_required {
                    if let SessionRuntime::Claude(handle) = &record.runtime {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart Claude session runtime: {err:#}"
                            ))
                        })?;
                    }
                    record.runtime = SessionRuntime::None;
                    record.pending_claude_approvals.clear();
                    record.runtime_reset_required = false;
                }

                let handle = match &record.runtime {
                    SessionRuntime::Claude(handle) => handle.clone(),
                    SessionRuntime::Codex(_) => {
                        return Err(ApiError::internal(
                            "unexpected Codex runtime attached to Claude session",
                        ));
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to Claude session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_claude_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                            record.session.model.clone(),
                            record
                                .session
                                .claude_approval_mode
                                .unwrap_or_else(default_claude_approval_mode),
                            record
                                .session
                                .claude_effort
                                .unwrap_or_else(default_claude_effort),
                            record.external_session_id.clone(),
                            None,
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent Claude session: {err:#}"
                            ))
                        })?;
                        record.runtime = SessionRuntime::Claude(handle.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentClaude {
                    command: ClaudePromptCommand {
                        attachments: attachments.clone(),
                        text: runtime_prompt.clone(),
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
            Agent::Codex => {
                if record.runtime_reset_required {
                    if let SessionRuntime::Codex(handle) = &record.runtime {
                        if let Some(shared_session) = &handle.shared_session {
                            shared_session.detach();
                        } else {
                            handle.kill().map_err(|err| {
                                ApiError::internal(format!(
                                    "failed to restart Codex session runtime: {err:#}"
                                ))
                            })?;
                        }
                    }
                    record.runtime = SessionRuntime::None;
                    record.pending_codex_approvals.clear();
                    record.pending_codex_user_inputs.clear();
                    record.pending_codex_mcp_elicitations.clear();
                    record.pending_codex_app_requests.clear();
                    record.runtime_reset_required = false;
                }

                let handle = match &record.runtime {
                    SessionRuntime::Codex(handle) => handle.clone(),
                    SessionRuntime::Claude(_) => {
                        return Err(ApiError::internal(
                            "unexpected Claude runtime attached to Codex session",
                        ));
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to Codex session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_codex_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent Codex session: {err:#}"
                            ))
                        })?;
                        record.runtime = SessionRuntime::Codex(handle.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentCodex {
                    command: CodexPromptCommand {
                        approval_policy: record.codex_approval_policy,
                        attachments,
                        cwd: record.session.workdir.clone(),
                        model: record.session.model.clone(),
                        prompt: runtime_prompt.to_owned(),
                        reasoning_effort: record.codex_reasoning_effort,
                        resume_thread_id: record.external_session_id.clone(),
                        sandbox_mode: record.codex_sandbox_mode,
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
            agent @ (Agent::Cursor | Agent::Gemini) => {
                if !attachments.is_empty() {
                    return Err(ApiError::bad_request(format!(
                        "{} sessions do not support image attachments yet",
                        agent.name()
                    )));
                }

                if record.runtime_reset_required {
                    if let SessionRuntime::Acp(handle) = &record.runtime {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart {} session runtime: {err:#}",
                                agent.name()
                            ))
                        })?;
                    }
                    record.runtime = SessionRuntime::None;
                    record.pending_acp_approvals.clear();
                    record.runtime_reset_required = false;
                }

                let expected_acp_agent = agent
                    .acp_runtime()
                    .ok_or_else(|| ApiError::internal("missing ACP runtime config"))?;
                let handle = match &record.runtime {
                    SessionRuntime::Acp(handle) if handle.agent == expected_acp_agent => {
                        handle.clone()
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to session",
                        ));
                    }
                    SessionRuntime::Claude(_) => {
                        return Err(ApiError::internal(
                            "unexpected Claude runtime attached to ACP session",
                        ));
                    }
                    SessionRuntime::Codex(_) => {
                        return Err(ApiError::internal(
                            "unexpected Codex runtime attached to ACP session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_acp_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                            expected_acp_agent,
                            record.session.gemini_approval_mode,
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent {} session: {err:#}",
                                agent.name()
                            ))
                        })?;
                        record.runtime = SessionRuntime::Acp(handle.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentAcp {
                    command: AcpPromptCommand {
                        cwd: record.session.workdir.clone(),
                        cursor_mode: record.session.cursor_mode,
                        model: record.session.model.clone(),
                        prompt: runtime_prompt.to_owned(),
                        resume_session_id: record.external_session_id.clone(),
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
        };

        push_message_on_record(
            record,
            Message::Text {
                attachments: message_attachments.clone(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::You,
                text: prompt.clone(),
                expanded_text: expanded_prompt,
            },
        );
        record.session.status = SessionStatus::Active;
        record.session.preview = prompt_preview_text(&prompt, &message_attachments);

        Ok(dispatch)
    }

    fn dispatch_next_queued_turn(&self, session_id: &str) -> Result<Option<TurnDispatch>> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;

        let queued = inner.sessions[index].queued_prompts.front().cloned();

        let Some(queued) = queued else {
            return Ok(None);
        };

        let dispatch = self
            .start_turn_on_record(
                &mut inner.sessions[index],
                queued.pending_prompt.id.clone(),
                queued.pending_prompt.text.clone(),
                queued.attachments.clone(),
                queued.pending_prompt.expanded_text.clone(),
            )
            .map_err(|err| anyhow!("failed to dispatch queued prompt: {}", err.message))?;
        inner.sessions[index].queued_prompts.pop_front();
        sync_pending_prompts(&mut inner.sessions[index]);
        self.commit_locked(&mut inner)?;
        Ok(Some(dispatch))
    }

    fn dispatch_turn(
        &self,
        session_id: &str,
        request: SendMessageRequest,
    ) -> std::result::Result<DispatchTurnResult, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            self.proxy_remote_turn_dispatch(session_id, request)?;
            return Ok(DispatchTurnResult::Queued);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;

        let prompt = request.text.trim().to_owned();
        let expanded_prompt = request
            .expanded_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != prompt)
            .map(str::to_owned);
        let attachments = parse_prompt_image_attachments(&request.attachments)?;
        if prompt.is_empty() && attachments.is_empty() {
            return Err(ApiError::bad_request("prompt cannot be empty"));
        }
        if record_has_archived_codex_thread(&inner.sessions[index]) {
            return Err(ApiError::conflict(
                "the current Codex thread is archived; unarchive it before sending another prompt",
            ));
        }

        let session_is_busy = matches!(
            inner.sessions[index].session.status,
            SessionStatus::Active | SessionStatus::Approval
        );
        let has_queued_prompts = !inner.sessions[index].queued_prompts.is_empty();

        if session_is_busy || has_queued_prompts {
            let message_id = inner.next_message_id();
            queue_prompt_on_record(
                &mut inner.sessions[index],
                PendingPrompt {
                    attachments: attachments
                        .iter()
                        .map(|attachment| attachment.metadata.clone())
                        .collect(),
                    id: message_id,
                    timestamp: stamp_now(),
                    text: prompt,
                    expanded_text: expanded_prompt.clone(),
                },
                attachments,
            );
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            if session_is_busy {
                return Ok(DispatchTurnResult::Queued);
            }

            drop(inner);
            let dispatch = self
                .dispatch_next_queued_turn(session_id)
                .map_err(|err| {
                    ApiError::internal(format!("failed to dispatch queued turn: {err:#}"))
                })?
                .ok_or_else(|| ApiError::internal("queued prompt disappeared before dispatch"))?;
            return Ok(DispatchTurnResult::Dispatched(dispatch));
        }

        let message_id = inner.next_message_id();
        let dispatch = self.start_turn_on_record(
            &mut inner.sessions[index],
            message_id,
            prompt,
            attachments,
            expanded_prompt,
        )?;

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;

        Ok(DispatchTurnResult::Dispatched(dispatch))
    }

    fn update_session_settings(
        &self,
        session_id: &str,
        request: UpdateSessionSettingsRequest,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_session_settings(session_id, request);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        let mut claude_model_update: Option<(ClaudeRuntimeHandle, String)> = None;
        let mut claude_permission_mode_update: Option<(ClaudeRuntimeHandle, String)> = None;
        let mut acp_config_updates: Vec<(AcpRuntimeHandle, Value)> = Vec::new();

        match record.session.agent {
            agent if agent.supports_codex_prompt_settings() => {
                if request.claude_approval_mode.is_some() || request.claude_effort.is_some() {
                    return Err(ApiError::bad_request(
                        "Claude mode and effort can only be changed for Claude sessions",
                    ));
                }
                if request.cursor_mode.is_some() || request.gemini_approval_mode.is_some() {
                    return Err(ApiError::bad_request(
                        "Codex sessions do not support Cursor or Gemini settings",
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
                        "Cursor sessions only support model and mode settings",
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
                        "Gemini sessions only support model and approval mode settings",
                    ));
                }
            }
            agent => {
                if request.model.is_some()
                    || request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(format!(
                        "{} sessions do not support prompt settings yet",
                        agent.name()
                    )));
                }
            }
        }

        if let Some(name) = request.name.as_deref() {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Err(ApiError::bad_request("session name cannot be empty"));
            }
            record.session.name = trimmed.to_owned();
        }

        if let Some(model) = request.model.as_deref() {
            let trimmed = model.trim();
            if trimmed.is_empty() {
                return Err(ApiError::bad_request("session model cannot be empty"));
            }
        }
        let requested_model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                matching_session_model_option_value(value, &record.session.model_options)
                    .unwrap_or_else(|| value.to_owned())
            });

        match record.session.agent {
            agent if agent.supports_codex_prompt_settings() => {
                let next_model = requested_model
                    .clone()
                    .unwrap_or_else(|| record.session.model.clone());
                let next_reasoning_effort = request
                    .reasoning_effort
                    .unwrap_or(record.codex_reasoning_effort);
                let normalized_reasoning_effort = normalized_codex_reasoning_effort(
                    &next_model,
                    next_reasoning_effort,
                    &record.session.model_options,
                );
                if request.reasoning_effort.is_some() {
                    if let Some(normalized_reasoning_effort) = normalized_reasoning_effort {
                        if normalized_reasoning_effort != next_reasoning_effort {
                            if let Some(option) =
                                codex_model_option(&next_model, &record.session.model_options)
                            {
                                return Err(ApiError::bad_request(format!(
                                    "model `{}` does not support `{}` reasoning effort; choose {}",
                                    option.label,
                                    next_reasoning_effort.as_api_value(),
                                    format_codex_reasoning_efforts(
                                        &option.supported_reasoning_efforts
                                    )
                                )));
                            }
                        }
                    }
                }
                if let Some(model) = requested_model.as_deref() {
                    if record.session.model != model {
                        record.session.model = model.to_owned();
                    }
                }
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
                } else if let Some(normalized_reasoning_effort) = normalized_reasoning_effort {
                    if record.codex_reasoning_effort != normalized_reasoning_effort {
                        record.codex_reasoning_effort = normalized_reasoning_effort;
                        record.session.reasoning_effort = Some(normalized_reasoning_effort);
                    }
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                let should_restart_for_effort =
                    request.claude_effort.is_some_and(|claude_effort| {
                        record.session.claude_effort != Some(claude_effort)
                    });
                if should_restart_for_effort {
                    record.runtime_reset_required = true;
                }
                if let Some(model) = requested_model.as_deref() {
                    if record.session.model != model {
                        record.session.model = model.to_owned();
                        if should_restart_for_effort {
                            record.runtime_reset_required = true;
                        } else if let SessionRuntime::Claude(handle) = &record.runtime {
                            claude_model_update = Some((handle.clone(), model.to_owned()));
                        }
                    }
                }
                if let Some(claude_approval_mode) = request.claude_approval_mode {
                    record.session.claude_approval_mode = Some(claude_approval_mode);
                    if let SessionRuntime::Claude(handle) = &record.runtime {
                        claude_permission_mode_update = Some((
                            handle.clone(),
                            claude_approval_mode
                                .session_cli_permission_mode()
                                .to_owned(),
                        ));
                    }
                }
                if let Some(claude_effort) = request.claude_effort {
                    record.session.claude_effort = Some(claude_effort);
                }
            }
            agent if agent.supports_cursor_mode() => {
                if let Some(model) = requested_model.as_deref() {
                    if record.session.model != model {
                        record.session.model = model.to_owned();
                        if let (SessionRuntime::Acp(handle), Some(external_session_id)) =
                            (&record.runtime, record.external_session_id.as_deref())
                        {
                            acp_config_updates.push((
                                handle.clone(),
                                json!({
                                    "id": Uuid::new_v4().to_string(),
                                    "method": "session/set_config_option",
                                    "params": {
                                        "sessionId": external_session_id,
                                        "optionId": "model",
                                        "value": model,
                                    }
                                }),
                            ));
                        }
                    }
                }
                if let Some(cursor_mode) = request.cursor_mode {
                    if record.session.cursor_mode != Some(cursor_mode) {
                        record.session.cursor_mode = Some(cursor_mode);
                        if let (SessionRuntime::Acp(handle), Some(external_session_id)) =
                            (&record.runtime, record.external_session_id.as_deref())
                        {
                            acp_config_updates.push((
                                handle.clone(),
                                json!({
                                    "id": Uuid::new_v4().to_string(),
                                    "method": "session/set_config_option",
                                    "params": {
                                        "sessionId": external_session_id,
                                        "optionId": "mode",
                                        "value": cursor_mode.as_acp_value(),
                                    }
                                }),
                            ));
                        }
                    }
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if let Some(model) = requested_model.as_deref() {
                    record.session.model = model.to_owned();
                }
                if let Some(gemini_approval_mode) = request.gemini_approval_mode {
                    if record.session.gemini_approval_mode != Some(gemini_approval_mode) {
                        record.runtime_reset_required = true;
                    }
                    record.session.gemini_approval_mode = Some(gemini_approval_mode);
                }
            }
            _ => {}
        }

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        let snapshot = self.snapshot_from_inner(&inner);
        drop(inner);

        if let Some((handle, model)) = claude_model_update {
            let _ = handle.input_tx.send(ClaudeRuntimeCommand::SetModel(model));
        }
        if let Some((handle, permission_mode)) = claude_permission_mode_update {
            let _ = handle
                .input_tx
                .send(ClaudeRuntimeCommand::SetPermissionMode(permission_mode));
        }
        for (handle, request) in acp_config_updates {
            let _ = handle
                .input_tx
                .send(AcpRuntimeCommand::JsonRpcMessage(request));
        }

        Ok(snapshot)
    }

    fn refresh_session_model_options(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_refresh_session_model_options(session_id);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        let agent = record.session.agent;
        if agent == Agent::Claude {
            if record.runtime_reset_required {
                if let SessionRuntime::Claude(handle) = &record.runtime {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!(
                            "failed to restart Claude session runtime: {err:#}"
                        ))
                    })?;
                }
                record.runtime = SessionRuntime::None;
                record.pending_claude_approvals.clear();
                record.runtime_reset_required = false;
            }

            match &record.runtime {
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::internal(
                        "unexpected ACP runtime attached to Claude session",
                    ));
                }
                SessionRuntime::Codex(_) => {
                    return Err(ApiError::internal(
                        "unexpected Codex runtime attached to Claude session",
                    ));
                }
                SessionRuntime::Claude(handle) => {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!(
                            "failed to restart Claude session runtime: {err:#}"
                        ))
                    })?;
                    record.runtime = SessionRuntime::None;
                    record.pending_claude_approvals.clear();
                }
                SessionRuntime::None => {}
            }

            let (response_tx, response_rx) =
                mpsc::channel::<std::result::Result<Vec<SessionModelOption>, String>>();
            let handle = spawn_claude_runtime(
                self.clone(),
                record.session.id.clone(),
                record.session.workdir.clone(),
                record.session.model.clone(),
                record
                    .session
                    .claude_approval_mode
                    .unwrap_or_else(default_claude_approval_mode),
                record
                    .session
                    .claude_effort
                    .unwrap_or_else(default_claude_effort),
                record.external_session_id.clone(),
                Some(response_tx),
            )
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to start persistent Claude session: {err:#}"
                ))
            })?;
            record.runtime = SessionRuntime::Claude(handle);
            drop(inner);

            let model_options = match response_rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(model_options)) => model_options,
                Ok(Err(detail)) => {
                    return Err(ApiError::internal(format!(
                        "failed to refresh Claude model options: {detail}"
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ApiError::internal(
                        "timed out refreshing Claude model options".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ApiError::internal(
                        "Claude model refresh did not return a result".to_owned(),
                    ));
                }
            };

            self.sync_session_model_options(session_id, None, model_options)
                .map_err(|err| {
                    ApiError::internal(format!("failed to sync Claude model options: {err:#}"))
                })?;
            return Ok(self.snapshot());
        }

        if agent == Agent::Codex {
            if record.runtime_reset_required {
                if let SessionRuntime::Codex(handle) = &record.runtime {
                    if let Some(shared_session) = &handle.shared_session {
                        shared_session.detach();
                    } else {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart Codex session runtime: {err:#}"
                            ))
                        })?;
                    }
                }
                record.runtime = SessionRuntime::None;
                record.pending_codex_approvals.clear();
                record.pending_codex_user_inputs.clear();
                record.pending_codex_mcp_elicitations.clear();
                record.pending_codex_app_requests.clear();
                record.runtime_reset_required = false;
            }

            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::internal(
                        "unexpected ACP runtime attached to Codex session",
                    ));
                }
                SessionRuntime::Claude(_) => {
                    return Err(ApiError::internal(
                        "unexpected Claude runtime attached to Codex session",
                    ));
                }
                SessionRuntime::None => {
                    let handle = spawn_codex_runtime(
                        self.clone(),
                        record.session.id.clone(),
                        record.session.workdir.clone(),
                    )
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to start persistent Codex session: {err:#}"
                        ))
                    })?;
                    record.runtime = SessionRuntime::Codex(handle.clone());
                    handle
                }
            };
            drop(inner);

            let (response_tx, response_rx) =
                mpsc::channel::<std::result::Result<Vec<SessionModelOption>, String>>();
            handle
                .input_tx
                .send(CodexRuntimeCommand::RefreshModelList { response_tx })
                .map_err(|err| {
                    ApiError::internal(format!("failed to queue Codex model refresh: {err}"))
                })?;

            let model_options = match response_rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(model_options)) => model_options,
                Ok(Err(detail)) => {
                    return Err(ApiError::internal(format!(
                        "failed to refresh Codex model options: {detail}"
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ApiError::internal(
                        "timed out refreshing Codex model options".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ApiError::internal(
                        "Codex model refresh did not return a result".to_owned(),
                    ));
                }
            };

            self.sync_session_model_options(session_id, None, model_options)
                .map_err(|err| {
                    ApiError::internal(format!("failed to sync Codex model options: {err:#}"))
                })?;
            return Ok(self.snapshot());
        }

        let expected_acp_agent = agent.acp_runtime().ok_or_else(|| {
            ApiError::bad_request(format!(
                "{} sessions do not expose live model options",
                agent.name()
            ))
        })?;

        if record.runtime_reset_required {
            if let SessionRuntime::Acp(handle) = &record.runtime {
                handle.kill().map_err(|err| {
                    ApiError::internal(format!(
                        "failed to restart {} session runtime: {err:#}",
                        agent.name()
                    ))
                })?;
            }
            record.runtime = SessionRuntime::None;
            record.pending_acp_approvals.clear();
            record.runtime_reset_required = false;
        }

        let handle = match &record.runtime {
            SessionRuntime::Acp(handle) if handle.agent == expected_acp_agent => handle.clone(),
            SessionRuntime::Acp(_) => {
                return Err(ApiError::internal(
                    "unexpected ACP runtime attached to session",
                ));
            }
            SessionRuntime::Claude(_) => {
                return Err(ApiError::internal(
                    "unexpected Claude runtime attached to ACP session",
                ));
            }
            SessionRuntime::Codex(_) => {
                return Err(ApiError::internal(
                    "unexpected Codex runtime attached to ACP session",
                ));
            }
            SessionRuntime::None => {
                let handle = spawn_acp_runtime(
                    self.clone(),
                    record.session.id.clone(),
                    record.session.workdir.clone(),
                    expected_acp_agent,
                    record.session.gemini_approval_mode,
                )
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to start persistent {} session: {err:#}",
                        agent.name()
                    ))
                })?;
                record.runtime = SessionRuntime::Acp(handle.clone());
                handle
            }
        };

        let command = AcpPromptCommand {
            cwd: record.session.workdir.clone(),
            cursor_mode: record.session.cursor_mode,
            model: record.session.model.clone(),
            prompt: String::new(),
            resume_session_id: record.external_session_id.clone(),
        };
        drop(inner);

        let (response_tx, response_rx) = mpsc::channel::<std::result::Result<(), String>>();
        handle
            .input_tx
            .send(AcpRuntimeCommand::RefreshSessionConfig {
                command,
                response_tx,
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to queue {} model refresh: {err}",
                    agent.name()
                ))
            })?;

        match response_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => Ok(self.snapshot()),
            Ok(Err(detail)) => Err(ApiError::internal(format!(
                "failed to refresh {} model options: {detail}",
                agent.name()
            ))),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ApiError::internal(format!(
                "timed out refreshing {} model options",
                agent.name()
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ApiError::internal(format!(
                "{} model refresh did not return a result",
                agent.name()
            ))),
        }
    }

    fn fork_codex_thread(&self, session_id: &str) -> std::result::Result<CreateSessionResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_fork_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        let fork_result = self.perform_codex_json_rpc_request(
            "thread/fork",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let fork_thread_id = fork_result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::internal("Codex thread fork did not return a thread id"))?
            .to_owned();
        let fork_name = default_forked_codex_session_name(
            &context.name,
            fork_result.pointer("/thread/name").and_then(Value::as_str),
        );
        let fork_model = fork_result
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&context.model)
            .to_owned();
        let fork_workdir = resolve_forked_codex_workdir(
            fork_result.get("cwd").and_then(Value::as_str),
            &context.workdir,
            context.project_id.as_deref(),
            self,
        )?;
        let approval_policy = fork_result
            .get("approvalPolicy")
            .and_then(codex_approval_policy_from_json_value)
            .unwrap_or(context.approval_policy);
        let sandbox_mode = fork_result
            .get("sandbox")
            .and_then(codex_sandbox_mode_from_json_value)
            .unwrap_or(context.sandbox_mode);
        let reasoning_effort = fork_result
            .get("reasoningEffort")
            .and_then(codex_reasoning_effort_from_json_value)
            .unwrap_or(context.reasoning_effort);
        let fork_preview = fork_result
            .pointer("/thread/preview")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(make_preview);
        let fork_thread = fork_result
            .get("thread")
            .ok_or_else(|| ApiError::internal("Codex thread fork did not return a thread"))?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let fork_messages = codex_thread_messages_from_json(&mut inner, fork_thread);
        let mut record = inner.create_session(
            Agent::Codex,
            Some(fork_name),
            fork_workdir,
            context.project_id.clone(),
            Some(fork_model),
        );
        record.session.model_options = context.model_options.clone();
        record.codex_approval_policy = approval_policy;
        record.session.approval_policy = Some(approval_policy);
        record.codex_sandbox_mode = sandbox_mode;
        record.session.sandbox_mode = Some(sandbox_mode);
        record.codex_reasoning_effort = reasoning_effort;
        record.session.reasoning_effort = Some(reasoning_effort);
        set_record_external_session_id(&mut record, Some(fork_thread_id.clone()));
        if let Some(fork_messages) = fork_messages {
            replace_session_messages_on_record(&mut record, fork_messages, fork_preview);
        } else {
            let note_message_id = inner.next_message_id();
            push_session_markdown_note_on_record(
                &mut record,
                note_message_id,
                "Forked Codex thread",
                format!(
                    "Forked from `{}` into live Codex thread `{}`.\n\nPreview: {}\n\nCodex did not return the earlier thread history for this fork, so TermAl could not backfill the transcript. New prompts here continue on the forked thread from this point forward.",
                    context.name,
                    fork_thread_id,
                    fork_preview
                        .as_deref()
                        .unwrap_or("No thread preview was returned.")
                ),
            );
        }

        if let Some(slot) = inner
            .find_session_index(&record.session.id)
            .and_then(|index| inner.sessions.get_mut(index))
        {
            *slot = record.clone();
        }
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist forked Codex session: {err:#}"))
        })?;

        Ok(CreateSessionResponse {
            session_id: record.session.id,
            state: self.snapshot_from_inner(&inner),
        })
    }

    fn archive_codex_thread(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_archive_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        if context.thread_state == Some(CodexThreadState::Archived) {
            return Err(ApiError::conflict("the current Codex thread is already archived"));
        }
        self.perform_codex_json_rpc_request(
            "thread/archive",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let note_message_id = inner.next_message_id();
        set_record_codex_thread_state(&mut inner.sessions[index], CodexThreadState::Archived);
        push_session_markdown_note_on_record(
            &mut inner.sessions[index],
            note_message_id,
            "Archived Codex thread",
            format!(
                "Archived the live Codex thread `{}`.\n\nUse **Unarchive** to restore it later before sending more prompts.",
                context.thread_id
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist archived Codex thread note: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    fn unarchive_codex_thread(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_unarchive_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        if context.thread_state != Some(CodexThreadState::Archived) {
            return Err(ApiError::conflict("the current Codex thread is not archived"));
        }
        self.perform_codex_json_rpc_request(
            "thread/unarchive",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let note_message_id = inner.next_message_id();
        set_record_codex_thread_state(&mut inner.sessions[index], CodexThreadState::Active);
        push_session_markdown_note_on_record(
            &mut inner.sessions[index],
            note_message_id,
            "Restored Codex thread",
            format!(
                "Restored the archived Codex thread `{}` so the session can continue using it.",
                context.thread_id
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist restored Codex thread note: {err:#}"
            ))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    fn compact_codex_thread(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_compact_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        self.perform_codex_json_rpc_request(
            "thread/compact/start",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let note_message_id = inner.next_message_id();
        push_session_markdown_note_on_record(
            &mut inner.sessions[index],
            note_message_id,
            "Started Codex compaction",
            format!(
                "Started Codex context compaction for live thread `{}`.\n\nThe TermAl transcript stays intact, but the live Codex thread may now rely on a compacted summary internally.",
                context.thread_id
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist Codex compaction note: {err:#}"
            ))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    fn rollback_codex_thread(
        &self,
        session_id: &str,
        num_turns: usize,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_rollback_codex_thread(session_id, num_turns);
        }
        if num_turns == 0 {
            return Err(ApiError::bad_request("rollback requires at least one turn"));
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        let rollback_result = self.perform_codex_json_rpc_request(
            "thread/rollback",
            json!({
                "threadId": context.thread_id,
                "numTurns": num_turns,
            }),
            Duration::from_secs(30),
        )?;
        let rollback_thread = rollback_result
            .get("thread")
            .ok_or_else(|| ApiError::internal("Codex thread rollback did not return a thread"))?;
        let rollback_preview = rollback_thread
            .get("preview")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(make_preview);

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let rollback_messages = codex_thread_messages_from_json(&mut inner, rollback_thread);
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        if let Some(rollback_messages) = rollback_messages {
            replace_session_messages_on_record(
                &mut inner.sessions[index],
                rollback_messages,
                rollback_preview,
            );
        } else {
            let turn_label = if num_turns == 1 { "turn" } else { "turns" };
            let note_message_id = inner.next_message_id();
            push_session_markdown_note_on_record(
                &mut inner.sessions[index],
                note_message_id,
                "Rolled back Codex thread",
                format!(
                    "Rolled back the live Codex thread `{}` by {} {}.\n\nCodex did not return the updated thread history for this rollback, so TermAl kept the earlier local transcript above. It may not exactly match the live Codex thread after this point.",
                    context.thread_id, num_turns, turn_label
                ),
            );
        }
        inner.sessions[index].session.status = SessionStatus::Idle;
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist Codex rollback state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    fn allocate_message_id(&self) -> String {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner.next_message_id()
    }

    fn set_external_session_id(&self, session_id: &str, external_session_id: String) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        set_record_external_session_id(&mut inner.sessions[index], Some(external_session_id));
        if inner.sessions[index]
            .session
            .agent
            .supports_codex_prompt_settings()
        {
            let external_session_id = inner.sessions[index].external_session_id.clone();
            inner.allow_discovered_codex_thread(external_session_id.as_deref());
        }
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn set_codex_thread_state_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        thread_state: CodexThreadState,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = &mut inner.sessions[index];
        if !record.runtime.matches_runtime_token(token) {
            return Ok(());
        }

        let next_state = normalized_codex_thread_state(
            record.session.agent,
            record.external_session_id.as_deref(),
            Some(thread_state),
        );
        if record.session.codex_thread_state == next_state {
            return Ok(());
        }

        record.session.codex_thread_state = next_state;
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn sync_session_model_options(
        &self,
        session_id: &str,
        current_model: Option<String>,
        model_options: Vec<SessionModelOption>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = &mut inner.sessions[index];

        let mut changed = false;
        if let Some(current_model) = current_model
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            if record.session.model != current_model {
                record.session.model = current_model;
                changed = true;
            }
        }
        if record.session.model_options != model_options {
            record.session.model_options = model_options;
            changed = true;
        }
        if record.session.agent.supports_codex_prompt_settings() {
            if let Some(normalized_effort) = normalized_codex_reasoning_effort(
                &record.session.model,
                record.codex_reasoning_effort,
                &record.session.model_options,
            ) {
                if record.codex_reasoning_effort != normalized_effort {
                    record.codex_reasoning_effort = normalized_effort;
                    record.session.reasoning_effort = Some(normalized_effort);
                    changed = true;
                }
            }
        }

        if changed {
            self.commit_locked(&mut inner)?;
        }
        Ok(())
    }

    fn sync_session_agent_commands(
        &self,
        session_id: &str,
        agent_commands: Vec<AgentCommand>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let next_commands = dedupe_agent_commands(agent_commands);
        let should_publish = {
            let record = &mut inner.sessions[index];
            if record.agent_commands == next_commands {
                return Ok(());
            }
            record.agent_commands = next_commands;
            if record.hidden {
                false
            } else {
                record.session.agent_commands_revision =
                    record.session.agent_commands_revision.saturating_add(1);
                true
            }
        };
        if should_publish {
            self.commit_locked(&mut inner)?;
        }
        Ok(())
    }

    fn sync_session_cursor_mode(
        &self,
        session_id: &str,
        cursor_mode: Option<CursorMode>,
    ) -> Result<()> {
        let Some(cursor_mode) = cursor_mode else {
            return Ok(());
        };

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = &mut inner.sessions[index];
        if !record.session.agent.supports_cursor_mode()
            || record.session.cursor_mode == Some(cursor_mode)
        {
            return Ok(());
        }

        record.session.cursor_mode = Some(cursor_mode);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn note_codex_rate_limits(&self, rate_limits: CodexRateLimits) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner.codex.rate_limits.as_ref() == Some(&rate_limits) {
            return Ok(());
        }

        inner.codex.rate_limits = Some(rate_limits);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn note_codex_notice(&self, notice: CodexNotice) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner
            .codex
            .notices
            .first()
            .is_some_and(|existing| same_codex_notice_identity(existing, &notice))
        {
            return Ok(());
        }

        if let Some(index) = inner
            .codex
            .notices
            .iter()
            .position(|existing| same_codex_notice_identity(existing, &notice))
        {
            inner.codex.notices.remove(index);
        }

        inner.codex.notices.insert(0, notice);
        inner.codex.notices.truncate(5);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn record_codex_runtime_config(
        &self,
        session_id: &str,
        sandbox_mode: CodexSandboxMode,
        approval_policy: CodexApprovalPolicy,
        reasoning_effort: CodexReasoningEffort,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index].active_codex_sandbox_mode = Some(sandbox_mode);
        inner.sessions[index].active_codex_approval_policy = Some(approval_policy);
        inner.sessions[index].active_codex_reasoning_effort = Some(reasoning_effort);
        self.persist_internal_locked(&inner)?;
        Ok(())
    }

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

    fn session_matches_runtime_token(&self, session_id: &str, token: &RuntimeToken) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions.get(index))
            .is_some_and(|record| record.runtime.matches_runtime_token(token))
    }

    fn clear_runtime(&self, session_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = &mut inner.sessions[index];
        let had_changes = !matches!(record.runtime, SessionRuntime::None)
            || record.runtime_reset_required
            || has_pending_requests(record);
        if !had_changes {
            return Ok(());
        }

        record.runtime = SessionRuntime::None;
        record.runtime_reset_required = false;
        clear_all_pending_requests(record);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn fail_turn_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: &str,
    ) -> Result<()> {
        let cleaned = error_message.trim();
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let message_id = (!cleaned.is_empty()).then(|| inner.next_message_id());
            let record = &mut inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }

            if let Some(message_id) = message_id {
                record.session.messages.push(Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Turn failed: {cleaned}"),
                    expanded_text: None,
                });
            }

            record.session.status = SessionStatus::Error;
            record.session.preview = make_preview(cleaned);
            self.commit_locked(&mut inner)?;
            true
        };

        if should_dispatch_next {
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        }

        Ok(())
    }

    fn note_turn_retry_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        detail: &str,
    ) -> Result<()> {
        let cleaned = detail.trim();
        if cleaned.is_empty() {
            return Ok(());
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;

        let duplicate_last_message = {
            let record = &inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }

            matches!(
                record.session.messages.last(),
                Some(Message::Text {
                    author: Author::Assistant,
                    text,
                    ..
                }) if text.trim() == cleaned
            )
        };

        let message_id = (!duplicate_last_message).then(|| inner.next_message_id());
        let record = &mut inner.sessions[index];

        if let Some(message_id) = message_id {
            record.session.messages.push(Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: cleaned.to_owned(),
                expanded_text: None,
            });
        }

        if record.session.status != SessionStatus::Approval {
            record.session.status = SessionStatus::Active;
        }
        record.session.preview = make_preview(cleaned);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn mark_turn_error_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: &str,
    ) -> Result<()> {
        let cleaned = error_message.trim();
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let record = &mut inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }

            record.session.status = SessionStatus::Error;
            if !cleaned.is_empty() {
                record.session.preview = make_preview(cleaned);
            }
            let has_queued_prompts = !record.queued_prompts.is_empty();
            self.commit_locked(&mut inner)?;
            has_queued_prompts
        };

        if should_dispatch_next {
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        }

        Ok(())
    }

    fn finish_turn_ok_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
    ) -> Result<()> {
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let record = &mut inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }

            if record.session.status == SessionStatus::Active {
                record.session.status = SessionStatus::Idle;
            }
            if record.session.preview.trim().is_empty() {
                record.session.preview = "Turn completed.".to_owned();
            }
            self.commit_locked(&mut inner)?;
            true
        };

        if should_dispatch_next {
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        }

        Ok(())
    }

    fn handle_runtime_exit_if_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: Option<&str>,
    ) -> Result<()> {
        let cleaned = error_message.map(str::trim).unwrap_or("");
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let matches_runtime = inner.sessions[index].runtime.matches_runtime_token(token);
            if !matches_runtime {
                return Ok(());
            }
            let was_busy = matches!(
                inner.sessions[index].session.status,
                SessionStatus::Active | SessionStatus::Approval
            );
            let message_id = (was_busy || !cleaned.is_empty()).then(|| inner.next_message_id());
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::None;
            record.runtime_reset_required = false;
            clear_all_pending_requests(record);

            if !cleaned.is_empty() || was_busy {
                let detail = if !cleaned.is_empty() {
                    cleaned.to_owned()
                } else {
                    match token {
                        RuntimeToken::Claude(_) => {
                            "Claude session exited before the active turn completed".to_owned()
                        }
                        RuntimeToken::Codex(_) => {
                            "Codex session exited before the active turn completed".to_owned()
                        }
                        RuntimeToken::Acp(_) => {
                            "Agent session exited before the active turn completed".to_owned()
                        }
                    }
                };
                if let Some(message_id) = message_id {
                    record.session.messages.push(Message::Text {
                        attachments: Vec::new(),
                        id: message_id,
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        text: format!("Turn failed: {detail}"),
                        expanded_text: None,
                    });
                }
                record.session.status = SessionStatus::Error;
                record.session.preview = make_preview(&detail);
            }

            let has_queued_prompts = !record.queued_prompts.is_empty();
            self.commit_locked(&mut inner)?;
            has_queued_prompts
        };

        if should_dispatch_next {
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        }

        Ok(())
    }

    fn register_claude_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: ClaudePendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_claude_approvals
            .insert(message_id, approval);
        Ok(())
    }

    fn register_codex_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_approvals
            .insert(message_id, approval);
        Ok(())
    }

    fn register_codex_pending_user_input(
        &self,
        session_id: &str,
        message_id: String,
        request: CodexPendingUserInput,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_user_inputs
            .insert(message_id, request);
        Ok(())
    }

    fn register_codex_pending_mcp_elicitation(
        &self,
        session_id: &str,
        message_id: String,
        request: CodexPendingMcpElicitation,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_mcp_elicitations
            .insert(message_id, request);
        Ok(())
    }

    fn register_codex_pending_app_request(
        &self,
        session_id: &str,
        message_id: String,
        request: CodexPendingAppRequest,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_app_requests
            .insert(message_id, request);
        Ok(())
    }

    fn register_acp_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: AcpPendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_acp_approvals
            .insert(message_id, approval);
        Ok(())
    }

    fn clear_claude_pending_approval_by_request(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = &mut inner.sessions[index];
        let message_ids: Vec<String> = record
            .pending_claude_approvals
            .iter()
            .filter(|(_, approval)| approval.request_id == request_id)
            .map(|(message_id, _)| message_id.clone())
            .collect();

        if message_ids.is_empty() {
            return Ok(());
        }

        for message_id in &message_ids {
            set_approval_decision_on_record(record, message_id, ApprovalDecision::Canceled)?;
            record.pending_claude_approvals.remove(message_id);
        }

        sync_session_interaction_state(
            record,
            approval_preview_text(record.session.agent.name(), ApprovalDecision::Canceled),
        );
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn kill_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_kill_session(session_id);
        }
        let (snapshot, runtime_to_kill, hidden_runtimes_to_kill) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let workdir = inner.sessions[index].session.workdir.clone();
            let project_id = inner.sessions[index].session.project_id.clone();
            let agent = inner.sessions[index].session.agent;
            let external_session_id = inner.sessions[index].external_session_id.clone();
            let record = &mut inner.sessions[index];

            let runtime = match &record.runtime {
                SessionRuntime::Claude(handle) => Some(KillableRuntime::Claude(handle.clone())),
                SessionRuntime::Codex(handle) => Some(KillableRuntime::Codex(handle.clone())),
                SessionRuntime::Acp(handle) => Some(KillableRuntime::Acp(handle.clone())),
                SessionRuntime::None => None,
            };
            inner.sessions.remove(index);

            let mut hidden_runtimes = Vec::new();
            if agent == Agent::Claude {
                let visible_profiles = inner
                    .sessions
                    .iter()
                    .filter(|session_record| {
                        !session_record.hidden
                            && !session_record.is_remote_proxy()
                            && session_record.session.agent == Agent::Claude
                            && session_record.session.workdir == workdir
                            && session_record.session.project_id == project_id
                    })
                    .map(claude_spare_profile)
                    .collect::<Vec<_>>();
                inner.sessions.retain(|session_record| {
                    let should_consider = session_record.hidden
                        && !session_record.is_remote_proxy()
                        && session_record.session.agent == Agent::Claude
                        && session_record.session.workdir == workdir
                        && session_record.session.project_id == project_id;
                    if !should_consider {
                        return true;
                    }

                    let keep = visible_profiles
                        .iter()
                        .any(|profile| *profile == claude_spare_profile(session_record));
                    if !keep {
                        if let SessionRuntime::Claude(handle) = &session_record.runtime {
                            hidden_runtimes.push(KillableRuntime::Claude(handle.clone()));
                        }
                    }
                    keep
                });
            }

            if agent.supports_codex_prompt_settings() {
                inner.ignore_discovered_codex_thread(external_session_id.as_deref());
            }

            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            (
                self.snapshot_from_inner(&inner),
                runtime,
                hidden_runtimes,
            )
        };

        if let Some(runtime) = runtime_to_kill {
            if let Err(err) = shutdown_removed_runtime(runtime, &format!("session `{session_id}`")) {
                eprintln!("session cleanup warning> {err:#}");
            }
        }
        for runtime in hidden_runtimes_to_kill {
            if let Err(err) = shutdown_removed_runtime(runtime, "a hidden Claude spare") {
                eprintln!("session cleanup warning> {err:#}");
            }
        }

        Ok(snapshot)
    }

    fn cancel_queued_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_cancel_queued_prompt(session_id, prompt_id);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        let original_len = record.queued_prompts.len();
        record
            .queued_prompts
            .retain(|queued| queued.pending_prompt.id != prompt_id);
        if record.queued_prompts.len() == original_len {
            return Err(ApiError::not_found("queued prompt not found"));
        }
        sync_pending_prompts(record);

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    fn stop_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_stop_session(session_id);
        }
        let runtime_to_stop = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &mut inner.sessions[index];

            if !matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval
            ) {
                return Err(ApiError::conflict("session is not currently running"));
            }

            cancel_pending_interaction_messages(&mut record.session.messages);

            let runtime = match &record.runtime {
                SessionRuntime::Claude(handle) => KillableRuntime::Claude(handle.clone()),
                SessionRuntime::Codex(handle) => KillableRuntime::Codex(handle.clone()),
                SessionRuntime::Acp(handle) => KillableRuntime::Acp(handle.clone()),
                SessionRuntime::None => {
                    return Err(ApiError::conflict("session is not currently running"));
                }
            };

            record.session.status = SessionStatus::Idle;
            record.session.preview = "Stopping turn\u{2026}".to_owned();
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;

            runtime
        };

        let shared_interrupt_warning = match runtime_to_stop {
            KillableRuntime::Codex(handle) => {
                if let Some(shared_session) = &handle.shared_session {
                    shared_session.interrupt_and_detach().err()
                } else {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!("failed to stop session: {err:#}"))
                    })?;
                    None
                }
            }
            runtime => {
                runtime
                    .kill()
                    .map_err(|err| ApiError::internal(format!("failed to stop session: {err:#}")))?;
                None
            }
        };
        if let Some(err) = shared_interrupt_warning {
            eprintln!(
                "session cleanup warning> shared Codex stop interrupt failed for session `{session_id}`: {err:#}"
            );
        }
        self.clear_runtime(session_id)
            .map_err(|err| ApiError::internal(format!("failed to clear runtime: {err:#}")))?;
        self.push_message(
            session_id,
            Message::Text {
                attachments: Vec::new(),
                id: self.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Turn stopped by user.".to_owned(),
                expanded_text: None,
            },
        )
        .map_err(|err| ApiError::internal(format!("failed to record stop message: {err:#}")))?;

        if let Some(dispatch) = self.dispatch_next_queued_turn(session_id).map_err(|err| {
            ApiError::internal(format!("failed to dispatch queued prompt: {err:#}"))
        })? {
            deliver_turn_dispatch(self, dispatch)?;
        }

        Ok(self.snapshot())
    }

    fn push_message(&self, session_id: &str, message: Message) -> Result<()> {
        let (revision, message, message_index, preview, status) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (message_index, preview, status) = {
                let record = &mut inner.sessions[index];
                if let Some(next_preview) = message.preview_text() {
                    record.session.preview = next_preview;
                }
                if matches!(
                    message,
                    Message::Approval { .. }
                        | Message::UserInputRequest { .. }
                        | Message::McpElicitationRequest { .. }
                        | Message::CodexAppRequest { .. }
                ) {
                    record.session.status = SessionStatus::Approval;
                }
                let message_index = push_message_on_record(record, message.clone());
                (
                    message_index,
                    record.session.preview.clone(),
                    record.session.status,
                )
            };
            let revision = self.commit_persisted_delta_locked(&mut inner)?;
            (revision, message, message_index, preview, status)
        };

        self.publish_delta(&DeltaEvent::MessageCreated {
            revision,
            session_id: session_id.to_owned(),
            message_id: message.id().to_owned(),
            message_index,
            message,
            preview,
            status,
        });
        Ok(())
    }

    pub(crate) fn last_message_id(&self, session_id: &str) -> Result<Option<String>> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        Ok(inner.sessions[index]
            .session
            .messages
            .last()
            .map(|message| message.id().to_owned()))
    }

    pub(crate) fn insert_message_before(
        &self,
        session_id: &str,
        anchor_message_id: &str,
        message: Message,
    ) -> Result<()> {
        let (revision, message, message_index, preview, status) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (message_index, preview, status) = {
                let record = &mut inner.sessions[index];
                let anchor_index = message_index_on_record(record, anchor_message_id).ok_or_else(|| {
                    anyhow!(
                        "session `{session_id}` anchor message `{anchor_message_id}` not found"
                    )
                })?;
                // This insertion path is currently reserved for subagent-result messages that do
                // not contribute preview/status text. Keep the existing session preview/status in
                // the emitted delta unless a future caller explicitly broadens that contract.
                let message_index = insert_message_on_record(record, anchor_index, message.clone());
                (
                    message_index,
                    record.session.preview.clone(),
                    record.session.status,
                )
            };
            let revision = self.commit_persisted_delta_locked(&mut inner)?;
            (revision, message, message_index, preview, status)
        };

        self.publish_delta(&DeltaEvent::MessageCreated {
            revision,
            session_id: session_id.to_owned(),
            message_id: message.id().to_owned(),
            message_index,
            message,
            preview,
            status,
        });
        Ok(())
    }

    fn append_text_delta(&self, session_id: &str, message_id: &str, delta: &str) -> Result<()> {
        let (preview, revision, message_index) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let record = &mut inner.sessions[index];
            let message_index = message_index_on_record(record, message_id).ok_or_else(|| {
                anyhow!("session `{session_id}` message `{message_id}` not found")
            })?;
            let session = &mut record.session;

            let mut preview = None;
            let Some(message) = session.messages.get_mut(message_index) else {
                return Err(anyhow!(
                    "session `{session_id}` message index `{message_index}` is out of bounds"
                ));
            };
            match message {
                Message::Text { id, text, .. } if id == message_id => {
                    text.push_str(delta);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        preview = Some(make_preview(trimmed));
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "session `{session_id}` message `{message_id}` is not a text message"
                    ));
                }
            }

            if let Some(next_preview) = preview.as_ref() {
                session.preview = next_preview.clone();
            }
            let revision = self.commit_delta_locked(&mut inner)?;
            (preview, revision, message_index)
        };

        self.publish_delta(&DeltaEvent::TextDelta {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            delta: delta.to_owned(),
            preview,
        });

        Ok(())
    }

    fn replace_text_message(&self, session_id: &str, message_id: &str, text: &str) -> Result<()> {
        let (preview, revision, message_index, replacement_text) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let record = &mut inner.sessions[index];
            let message_index = message_index_on_record(record, message_id).ok_or_else(|| {
                anyhow!("session `{session_id}` message `{message_id}` not found")
            })?;
            let session = &mut record.session;

            let mut preview = None;
            let Some(message) = session.messages.get_mut(message_index) else {
                return Err(anyhow!(
                    "session `{session_id}` message index `{message_index}` is out of bounds"
                ));
            };
            match message {
                Message::Text {
                    id,
                    text: current_text,
                    ..
                } if id == message_id => {
                    current_text.clear();
                    current_text.push_str(text);
                    let trimmed = current_text.trim();
                    if !trimmed.is_empty() {
                        preview = Some(make_preview(trimmed));
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "session `{session_id}` message `{message_id}` is not a text message"
                    ));
                }
            }

            if let Some(next_preview) = preview.as_ref() {
                session.preview = next_preview.clone();
            }
            let revision = self.commit_delta_locked(&mut inner)?;
            (preview, revision, message_index, text.to_owned())
        };

        self.publish_delta(&DeltaEvent::TextReplace {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            text: replacement_text,
            preview,
        });

        Ok(())
    }

    fn upsert_command_message(
        &self,
        session_id: &str,
        message_id: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        let command_language = Some(shell_language().to_owned());
        let output_language = infer_command_output_language(command).map(str::to_owned);

        let (preview, revision, message_index, created_message, session_status) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (message_index, created_message, preview, session_status) = {
                let record = &mut inner.sessions[index];
                let (message_index, created_message) = if let Some(message_index) =
                    message_index_on_record(record, message_id)
                {
                    let Some(message) = record.session.messages.get_mut(message_index) else {
                        return Err(anyhow!(
                            "session `{session_id}` message index `{message_index}` is out of bounds"
                        ));
                    };
                    match message {
                        Message::Command {
                            id,
                            command: existing_command,
                            command_language: existing_command_language,
                            output: existing_output,
                            output_language: existing_output_language,
                            status: existing_status,
                            ..
                        } if id == message_id => {
                            *existing_command = command.to_owned();
                            *existing_command_language = command_language.clone();
                            *existing_output = output.to_owned();
                            *existing_output_language = output_language.clone();
                            *existing_status = status;
                            (message_index, None)
                        }
                        _ => {
                            return Err(anyhow!(
                                "session `{session_id}` message `{message_id}` is not a command message"
                            ));
                        }
                    }
                } else {
                    let message = Message::Command {
                        id: message_id.to_owned(),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        command: command.to_owned(),
                        command_language: command_language.clone(),
                        output: output.to_owned(),
                        output_language: output_language.clone(),
                        status,
                    };
                    let message_index = push_message_on_record(record, message.clone());
                    (message_index, Some(message))
                };

                let preview = match status {
                    CommandStatus::Running => make_preview(&format!("Running {command}")),
                    CommandStatus::Success => {
                        if output.trim().is_empty() {
                            make_preview(&format!("Completed {command}"))
                        } else {
                            make_preview(output.trim())
                        }
                    }
                    CommandStatus::Error => {
                        if output.trim().is_empty() {
                            make_preview(&format!("Command failed: {command}"))
                        } else {
                            make_preview(output.trim())
                        }
                    }
                };
                record.session.preview = preview.clone();
                (
                    message_index,
                    created_message,
                    preview,
                    record.session.status,
                )
            };
            let revision = if created_message.is_some() {
                self.commit_persisted_delta_locked(&mut inner)?
            } else {
                self.commit_delta_locked(&mut inner)?
            };
            (
                preview,
                revision,
                message_index,
                created_message,
                session_status,
            )
        };

        if let Some(message) = created_message {
            self.publish_delta(&DeltaEvent::MessageCreated {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message,
                preview,
                status: session_status,
            });
        } else {
            self.publish_delta(&DeltaEvent::CommandUpdate {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                command: command.to_owned(),
                command_language,
                output: output.to_owned(),
                output_language,
                status,
                preview,
            });
        }

        Ok(())
    }

    fn upsert_parallel_agents_message(
        &self,
        session_id: &str,
        message_id: &str,
        agents: Vec<ParallelAgentProgress>,
    ) -> Result<()> {
        let (preview, revision, message_index, created_message, session_status) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (message_index, created_message, preview, session_status) = {
                let record = &mut inner.sessions[index];

                let (message_index, created_message) = if let Some(message_index) =
                    message_index_on_record(record, message_id)
                {
                    let Some(message) = record.session.messages.get_mut(message_index) else {
                        return Err(anyhow!(
                            "session `{session_id}` message index `{message_index}` is out of bounds"
                        ));
                    };
                    match message {
                        Message::ParallelAgents {
                            id,
                            agents: existing_agents,
                            ..
                        } if id == message_id => {
                            *existing_agents = agents.clone();
                            (message_index, None)
                        }
                        _ => {
                            return Err(anyhow!(
                                "session `{session_id}` message `{message_id}` is not a parallel-agents message"
                            ));
                        }
                    }
                } else {
                    let message = Message::ParallelAgents {
                        id: message_id.to_owned(),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        agents: agents.clone(),
                    };
                    let message_index = push_message_on_record(record, message.clone());
                    (message_index, Some(message))
                };

                let preview = parallel_agents_preview_text(&agents);
                record.session.preview = preview.clone();
                (
                    message_index,
                    created_message,
                    preview,
                    record.session.status,
                )
            };
            let revision = if created_message.is_some() {
                self.commit_persisted_delta_locked(&mut inner)?
            } else {
                self.commit_delta_locked(&mut inner)?
            };
            (
                preview,
                revision,
                message_index,
                created_message,
                session_status,
            )
        };

        if let Some(message) = created_message {
            self.publish_delta(&DeltaEvent::MessageCreated {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message,
                preview,
                status: session_status,
            });
        } else {
            self.publish_delta(&DeltaEvent::ParallelAgentsUpdate {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                agents,
                preview,
            });
        }

        Ok(())
    }

    fn update_approval(
        &self,
        session_id: &str,
        message_id: &str,
        decision: ApprovalDecision,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_update_approval(session_id, message_id, decision);
        }
        if matches!(
            decision,
            ApprovalDecision::Interrupted | ApprovalDecision::Canceled
        ) {
            return Err(ApiError::bad_request(
                "approval decisions cannot be marked interrupted or canceled manually",
            ));
        }

        let mut claude_runtime_action: Option<(ClaudeRuntimeHandle, ClaudePendingApproval)> = None;
        let mut codex_runtime_action: Option<(CodexRuntimeHandle, CodexPendingApproval)> = None;
        let mut acp_runtime_action: Option<(AcpRuntimeHandle, AcpPendingApproval)> = None;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        if record.session.status != SessionStatus::Approval {
            return Err(ApiError::conflict(
                "session is not currently awaiting approval",
            ));
        }

        if record.session.agent == Agent::Claude
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_claude_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Claude(handle) => handle.clone(),
                SessionRuntime::Codex(_) => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
                SessionRuntime::None => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
            };
            claude_runtime_action = Some((handle, pending));
        } else if record.session.agent == Agent::Codex
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_codex_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            codex_runtime_action = Some((handle, pending));
        } else if matches!(record.session.agent, Agent::Cursor | Agent::Gemini)
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_acp_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Acp(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::Codex(_) | SessionRuntime::None => {
                    return Err(ApiError::conflict("agent session is not currently running"));
                }
            };
            acp_runtime_action = Some((handle, pending));
        }

        drop(inner);

        if let Some((handle, pending)) = claude_runtime_action {
            if decision == ApprovalDecision::AcceptedForSession {
                if let Some(mode) = pending.permission_mode_for_session.clone() {
                    handle
                        .input_tx
                        .send(ClaudeRuntimeCommand::SetPermissionMode(mode))
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to update Claude permission mode: {err}"
                            ))
                        })?;
                }
            }

            let response = match decision {
                ApprovalDecision::Accepted | ApprovalDecision::AcceptedForSession => {
                    ClaudePermissionDecision::Allow {
                        request_id: pending.request_id.clone(),
                        updated_input: pending.tool_input.clone(),
                    }
                }
                ApprovalDecision::Rejected => ClaudePermissionDecision::Deny {
                    request_id: pending.request_id.clone(),
                    message: "User rejected this action in TermAl.".to_owned(),
                },
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => {
                    unreachable!("non-deliverable approval decisions are not sent")
                }
            };

            handle
                .input_tx
                .send(ClaudeRuntimeCommand::PermissionResponse(response))
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to Claude: {err}"
                    ))
                })?;
        }
        if let Some((handle, pending)) = codex_runtime_action {
            handle
                .input_tx
                .send(CodexRuntimeCommand::JsonRpcResponse {
                    response: CodexJsonRpcResponseCommand {
                        request_id: pending.request_id.clone(),
                        result: codex_approval_result(&pending.kind, decision),
                    },
                })
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to Codex: {err}"
                    ))
                })?;
        }
        if let Some((handle, pending)) = acp_runtime_action {
            let option_id = match decision {
                ApprovalDecision::Accepted => pending
                    .allow_once_option_id
                    .clone()
                    .or_else(|| pending.allow_always_option_id.clone())
                    .or_else(|| pending.reject_option_id.clone()),
                ApprovalDecision::AcceptedForSession => pending
                    .allow_always_option_id
                    .clone()
                    .or_else(|| pending.allow_once_option_id.clone())
                    .or_else(|| pending.reject_option_id.clone()),
                ApprovalDecision::Rejected => pending
                    .reject_option_id
                    .clone()
                    .or_else(|| pending.allow_once_option_id.clone())
                    .or_else(|| pending.allow_always_option_id.clone()),
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => None,
            }
            .ok_or_else(|| {
                ApiError::conflict("no approval option is available for this request")
            })?;

            handle
                .input_tx
                .send(AcpRuntimeCommand::JsonRpcMessage(json!({
                    "id": pending.request_id.clone(),
                    "result": {
                        "outcome": {
                            "outcome": "selected",
                            "optionId": option_id,
                        }
                    }
                })))
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to agent session: {err}"
                    ))
                })?;
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        if record.session.status != SessionStatus::Approval && decision == ApprovalDecision::Pending
        {
            return Err(ApiError::conflict(
                "session is not currently awaiting approval",
            ));
        }
        set_approval_decision_on_record(record, message_id, decision)
            .map_err(|_| ApiError::not_found("approval message not found"))?;

        if decision != ApprovalDecision::Pending {
            record.pending_claude_approvals.remove(message_id);
            record.pending_codex_approvals.remove(message_id);
            record.pending_acp_approvals.remove(message_id);
        }
        sync_session_interaction_state(
            record,
            approval_preview_text(record.session.agent.name(), decision),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    fn submit_codex_user_input(
        &self,
        session_id: &str,
        message_id: &str,
        answers: BTreeMap<String, Vec<String>>,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_submit_codex_user_input(session_id, message_id, answers);
        }

        let (handle, pending, response_answers, display_answers) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &mut inner.sessions[index];
            if record.session.status != SessionStatus::Approval {
                return Err(ApiError::conflict(
                    "session is not currently waiting for input",
                ));
            }
            if record.session.agent != Agent::Codex {
                return Err(ApiError::conflict(
                    "only Codex sessions currently support structured user input",
                ));
            }

            let pending = record
                .pending_codex_user_inputs
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("user input request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            let (response_answers, display_answers) =
                validate_codex_user_input_answers(&pending.questions, answers)?;
            (handle, pending, response_answers, display_answers)
        };

        handle
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id: pending.request_id.clone(),
                    result: json!({ "answers": response_answers }),
                },
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to deliver user input response to Codex: {err}"
                ))
            })?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        set_user_input_request_state_on_record(
            record,
            message_id,
            InteractionRequestState::Submitted,
            Some(display_answers),
        )
        .map_err(|_| ApiError::not_found("user input request not found"))?;
        record.pending_codex_user_inputs.remove(message_id);
        sync_session_interaction_state(
            record,
            user_input_request_preview_text(
                record.session.agent.name(),
                InteractionRequestState::Submitted,
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    fn submit_codex_mcp_elicitation(
        &self,
        session_id: &str,
        message_id: &str,
        action: McpElicitationAction,
        content: Option<Value>,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_submit_codex_mcp_elicitation(
                session_id,
                message_id,
                action,
                content,
            );
        }

        let (handle, pending, normalized_content) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &mut inner.sessions[index];
            if record.session.status != SessionStatus::Approval {
                return Err(ApiError::conflict(
                    "session is not currently waiting for input",
                ));
            }
            if record.session.agent != Agent::Codex {
                return Err(ApiError::conflict(
                    "only Codex sessions currently support MCP elicitation input",
                ));
            }

            let pending = record
                .pending_codex_mcp_elicitations
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("MCP elicitation request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            let normalized_content =
                validate_codex_mcp_elicitation_submission(&pending.request, action, content)?;
            (handle, pending, normalized_content)
        };

        handle
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id: pending.request_id.clone(),
                    result: json!({
                        "action": action,
                        "content": normalized_content
                    }),
                },
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to deliver MCP elicitation response to Codex: {err}"
                ))
            })?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        set_mcp_elicitation_request_state_on_record(
            record,
            message_id,
            InteractionRequestState::Submitted,
            Some(action),
            normalized_content.clone(),
        )
        .map_err(|_| ApiError::not_found("MCP elicitation request not found"))?;
        record.pending_codex_mcp_elicitations.remove(message_id);
        sync_session_interaction_state(
            record,
            mcp_elicitation_request_preview_text(
                record.session.agent.name(),
                InteractionRequestState::Submitted,
                Some(action),
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    fn submit_codex_app_request(
        &self,
        session_id: &str,
        message_id: &str,
        result: Value,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_submit_codex_app_request(session_id, message_id, result);
        }
        let result = validate_codex_app_request_result(result)?;

        let (handle, pending) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &mut inner.sessions[index];
            if record.session.status != SessionStatus::Approval {
                return Err(ApiError::conflict(
                    "session is not currently waiting for a Codex request response",
                ));
            }
            if record.session.agent != Agent::Codex {
                return Err(ApiError::conflict(
                    "only Codex sessions currently support generic app-server requests",
                ));
            }

            let pending = record
                .pending_codex_app_requests
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("Codex app request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            (handle, pending)
        };

        handle
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id: pending.request_id.clone(),
                    result: result.clone(),
                },
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to deliver generic Codex app request response: {err}"
                ))
            })?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &mut inner.sessions[index];
        set_codex_app_request_state_on_record(
            record,
            message_id,
            InteractionRequestState::Submitted,
            Some(result),
        )
        .map_err(|_| ApiError::not_found("Codex app request not found"))?;
        record.pending_codex_app_requests.remove(message_id);
        sync_session_interaction_state(
            record,
            codex_app_request_preview_text(
                record.session.agent.name(),
                InteractionRequestState::Submitted,
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    fn fail_turn(&self, session_id: &str, error_message: &str) -> Result<()> {
        let cleaned = error_message.trim();
        if !cleaned.is_empty() {
            self.push_message(
                session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: self.allocate_message_id(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Turn failed: {cleaned}"),
                    expanded_text: None,
                },
            )?;
        }

        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let session = &mut inner.sessions[index].session;
            session.status = SessionStatus::Error;
            session.preview = make_preview(cleaned);
            self.commit_locked(&mut inner)?;
        }

        if let Some(dispatch) = self.dispatch_next_queued_turn(session_id)? {
            deliver_turn_dispatch(self, dispatch).map_err(|err| {
                anyhow!("failed to deliver queued turn dispatch: {}", err.message)
            })?;
        }
        Ok(())
    }
}

fn codex_approval_result(kind: &CodexApprovalKind, decision: ApprovalDecision) -> Value {
    match kind {
        CodexApprovalKind::CommandExecution => match decision {
            ApprovalDecision::Accepted => json!({ "decision": "accept" }),
            ApprovalDecision::AcceptedForSession => json!({ "decision": "acceptForSession" }),
            ApprovalDecision::Rejected => json!({ "decision": "decline" }),
            ApprovalDecision::Pending
            | ApprovalDecision::Interrupted
            | ApprovalDecision::Canceled => {
                unreachable!("non-deliverable approval decisions are not sent to Codex")
            }
        },
        CodexApprovalKind::FileChange => match decision {
            ApprovalDecision::Accepted => json!({ "decision": "accept" }),
            ApprovalDecision::AcceptedForSession => json!({ "decision": "acceptForSession" }),
            ApprovalDecision::Rejected => json!({ "decision": "decline" }),
            ApprovalDecision::Pending
            | ApprovalDecision::Interrupted
            | ApprovalDecision::Canceled => {
                unreachable!("non-deliverable approval decisions are not sent to Codex")
            }
        },
        CodexApprovalKind::Permissions {
            requested_permissions,
        } => {
            let permissions = match decision {
                ApprovalDecision::Accepted | ApprovalDecision::AcceptedForSession => {
                    requested_permissions.clone()
                }
                ApprovalDecision::Rejected => json!({}),
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => {
                    unreachable!("non-deliverable approval decisions are not sent to Codex")
                }
            };
            let scope = match decision {
                ApprovalDecision::AcceptedForSession => "session",
                ApprovalDecision::Accepted | ApprovalDecision::Rejected => "turn",
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => {
                    unreachable!("non-deliverable approval decisions are not sent to Codex")
                }
            };
            json!({
                "permissions": permissions,
                "scope": scope,
            })
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiscoveredCodexThread {
    approval_policy: Option<CodexApprovalPolicy>,
    archived: bool,
    cwd: String,
    id: String,
    model: Option<String>,
    reasoning_effort: Option<CodexReasoningEffort>,
    sandbox_mode: Option<CodexSandboxMode>,
    title: String,
}

const MAX_DISCOVERED_CODEX_THREADS_PER_HOME: usize = 500;

fn collect_codex_discovery_scopes(default_workdir: &str, projects: &[Project]) -> Vec<PathBuf> {
    let mut scopes = Vec::new();
    let mut seen = HashSet::new();
    push_codex_home_candidate(
        &mut scopes,
        &mut seen,
        normalize_codex_discovery_path(FsPath::new(default_workdir)),
    );
    for project in projects {
        if project.remote_id == LOCAL_REMOTE_ID {
            push_codex_home_candidate(
                &mut scopes,
                &mut seen,
                normalize_codex_discovery_path(FsPath::new(&project.root_path)),
            );
        }
    }
    scopes
}

fn discover_codex_threads(
    default_workdir: &str,
    discovery_scopes: &[PathBuf],
) -> Result<Vec<DiscoveredCodexThread>> {
    let source_codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .map(|home| home.join(".codex"))
        })
        .or_else(|| Some(PathBuf::from(default_workdir).join(".codex")));
    let termal_codex_root = resolve_termal_codex_discovery_root(default_workdir);
    discover_codex_threads_from_sources(
        source_codex_home.as_deref(),
        &termal_codex_root,
        discovery_scopes,
    )
}

fn discover_codex_threads_from_sources(
    source_codex_home: Option<&FsPath>,
    termal_codex_root: &FsPath,
    discovery_scopes: &[PathBuf],
) -> Result<Vec<DiscoveredCodexThread>> {
    let codex_homes = discover_codex_home_candidates(source_codex_home, termal_codex_root);
    discover_codex_threads_from_homes(&codex_homes, discovery_scopes)
}

fn discover_codex_home_candidates(
    source_codex_home: Option<&FsPath>,
    termal_codex_root: &FsPath,
) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    let mut seen = HashSet::new();

    for scope in ["shared-app-server"] {
        push_codex_home_candidate(
            &mut homes,
            &mut seen,
            termal_codex_root.join(scope),
        );
    }

    let mut extra_termal_homes = fs::read_dir(termal_codex_root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| match entry.file_type() {
            Ok(file_type)
                if file_type.is_dir() && codex_home_scope_is_importable(&entry.path()) =>
            {
                Some(entry.path())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    extra_termal_homes.sort();
    for home in extra_termal_homes {
        push_codex_home_candidate(&mut homes, &mut seen, home);
    }

    push_codex_home_candidate(
        &mut homes,
        &mut seen,
        termal_codex_root.to_path_buf(),
    );

    if let Some(source_codex_home) = source_codex_home {
        push_codex_home_candidate(
            &mut homes,
            &mut seen,
            source_codex_home.to_path_buf(),
        );
    }

    homes
}

fn codex_home_scope_is_importable(path: &FsPath) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map_or(true, |scope| scope != "repl")
}

fn push_codex_home_candidate(
    homes: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    home: PathBuf,
) {
    let key = normalize_codex_discovery_path(&home);
    if seen.insert(key) {
        homes.push(home);
    }
}

fn resolve_termal_codex_discovery_root(default_workdir: &str) -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default_workdir))
        .join(".termal")
        .join("codex-home")
}

fn discover_codex_threads_from_homes(
    codex_homes: &[PathBuf],
    discovery_scopes: &[PathBuf],
) -> Result<Vec<DiscoveredCodexThread>> {
    let mut threads = Vec::new();
    let mut seen_ids = HashSet::new();

    for codex_home in codex_homes {
        for thread in discover_codex_threads_from_home(codex_home, discovery_scopes)? {
            if seen_ids.insert(thread.id.clone()) {
                threads.push(thread);
            }
        }
    }

    Ok(threads)
}

fn discover_codex_threads_from_home(
    codex_home: &FsPath,
    discovery_scopes: &[PathBuf],
) -> Result<Vec<DiscoveredCodexThread>> {
    let Some(database_path) = resolve_codex_threads_database_path(codex_home) else {
        return Ok(Vec::new());
    };
    if discovery_scopes.is_empty() {
        return Ok(Vec::new());
    }

    let connection = rusqlite::Connection::open_with_flags(
        &database_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .with_context(|| format!("failed to open `{}`", database_path.display()))?;
    let thread_columns = codex_threads_table_columns(&connection)?;
    let query_scopes = collect_codex_discovery_query_scope_strings(discovery_scopes);
    let query_scope_patterns = query_scopes
        .iter()
        .flat_map(|scope| codex_discovery_scope_query_patterns(scope))
        .collect::<Vec<_>>();
    let normalized_scopes = discovery_scopes
        .iter()
        .map(|scope| normalize_codex_discovery_path(scope))
        .collect::<Vec<_>>();
    let scope_sql = query_scope_patterns
        .iter()
        .map(|_| "(cwd = ? OR cwd LIKE ? ESCAPE '\\')")
        .collect::<Vec<_>>()
        .join(" OR ");
    let query = format!(
        "select id, cwd, title, {}, {}, {}, {}, {}
         from threads
         where {scope_sql}
         order by updated_at desc
         limit ?",
        codex_threads_select_column(&thread_columns, "sandbox_policy", "NULL"),
        codex_threads_select_column(&thread_columns, "approval_mode", "NULL"),
        codex_threads_select_column(&thread_columns, "archived", "0"),
        codex_threads_select_column(&thread_columns, "model", "NULL"),
        codex_threads_select_column(&thread_columns, "reasoning_effort", "NULL"),
    );
    let mut statement = connection.prepare(&query)?;
    let mut params = Vec::with_capacity((query_scope_patterns.len() * 2) + 1);
    for (scope, like_pattern) in &query_scope_patterns {
        params.push(rusqlite::types::Value::from(scope.clone()));
        params.push(rusqlite::types::Value::from(like_pattern.clone()));
    }
    params.push(rusqlite::types::Value::from(
        MAX_DISCOVERED_CODEX_THREADS_PER_HOME as i64,
    ));
    let rows = statement.query_map(rusqlite::params_from_iter(params), |row| {
        let sandbox_policy: Option<String> = row.get(3)?;
        let approval_mode: Option<String> = row.get(4)?;
        let model: Option<String> = row.get(6)?;
        let reasoning_effort: Option<String> = row.get(7)?;
        Ok(DiscoveredCodexThread {
            approval_policy: approval_mode
                .as_deref()
                .and_then(parse_discovered_codex_approval_policy),
            archived: row.get::<_, i64>(5)? != 0,
            cwd: row.get(1)?,
            id: row.get(0)?,
            model: model
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            reasoning_effort: reasoning_effort
                .as_deref()
                .and_then(parse_discovered_codex_reasoning_effort),
            sandbox_mode: sandbox_policy
                .as_deref()
                .and_then(parse_discovered_codex_sandbox_mode),
            title: row.get::<_, String>(2)?,
        })
    })?;

    let mut threads = Vec::new();
    for row in rows {
        let thread = row?;
        if thread.id.trim().is_empty() || thread.cwd.trim().is_empty() {
            continue;
        }
        if !normalized_scopes.iter().any(|scope| {
            codex_discovery_scope_contains(
                scope.to_string_lossy().as_ref(),
                FsPath::new(&thread.cwd),
            )
        }) {
            continue;
        }
        threads.push(thread);
    }
    Ok(threads)
}

fn codex_threads_table_columns(connection: &rusqlite::Connection) -> Result<HashSet<String>> {
    let mut statement = connection.prepare("pragma table_info(threads)")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = HashSet::new();
    for row in rows {
        columns.insert(row?);
    }
    Ok(columns)
}

fn codex_threads_select_column(
    columns: &HashSet<String>,
    column_name: &str,
    default_sql: &str,
) -> String {
    if columns.contains(column_name) {
        column_name.to_owned()
    } else {
        format!("{default_sql} as {column_name}")
    }
}

fn collect_codex_discovery_query_scope_strings(discovery_scopes: &[PathBuf]) -> Vec<String> {
    let mut scopes = Vec::new();
    let mut seen = HashSet::new();
    for scope in discovery_scopes {
        let raw = scope.to_string_lossy().to_string();
        if seen.insert(raw.clone()) {
            scopes.push(raw);
        }
        let normalized = normalize_codex_discovery_path(scope)
            .to_string_lossy()
            .to_string();
        if seen.insert(normalized.clone()) {
            scopes.push(normalized);
        }
    }
    scopes
}

fn codex_discovery_scope_query_patterns(scope: &str) -> Vec<(String, String)> {
    let mut patterns = Vec::new();
    let mut seen = HashSet::new();
    let mut candidates = vec![scope.to_owned()];
    if scope.contains('/') {
        candidates.push(scope.replace('/', "\\"));
    }
    if scope.contains('\\') {
        candidates.push(scope.replace('\\', "/"));
    }

    for candidate in candidates {
        if seen.insert(candidate.clone()) {
            patterns.push((
                candidate.clone(),
                codex_discovery_like_pattern(&candidate),
            ));
        }
    }

    patterns
}

fn codex_discovery_like_pattern(scope: &str) -> String {
    let escaped_scope = scope
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let separator = if scope.contains('\\') { '\\' } else { '/' };
    if escaped_scope.ends_with(separator) {
        format!("{escaped_scope}%")
    } else {
        format!("{escaped_scope}{separator}%")
    }
}

fn resolve_codex_threads_database_path(codex_home: &FsPath) -> Option<PathBuf> {
    let primary = codex_home.join("state.db");
    if primary
        .metadata()
        .ok()
        .filter(|metadata| metadata.is_file() && metadata.len() > 0)
        .is_some()
    {
        return Some(primary);
    }

    let mut best_candidate: Option<(u64, PathBuf)> = None;
    let entries = fs::read_dir(codex_home).ok()?;
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let version = name
            .strip_prefix("state_")
            .and_then(|value| value.strip_suffix(".sqlite"))
            .and_then(|value| value.parse::<u64>().ok());
        let Some(version) = version else {
            continue;
        };
        if !path.is_file() {
            continue;
        }

        match &best_candidate {
            Some((current_version, _)) if *current_version >= version => {}
            _ => {
                best_candidate = Some((version, path));
            }
        }
    }

    best_candidate.map(|(_, path)| path)
}

fn parse_discovered_codex_sandbox_mode(value: &str) -> Option<CodexSandboxMode> {
    let payload: Value = serde_json::from_str(value).ok()?;
    match payload.get("type").and_then(Value::as_str) {
        Some("read-only") => Some(CodexSandboxMode::ReadOnly),
        Some("workspace-write") => Some(CodexSandboxMode::WorkspaceWrite),
        Some("danger-full-access") => Some(CodexSandboxMode::DangerFullAccess),
        _ => None,
    }
}

fn parse_discovered_codex_approval_policy(value: &str) -> Option<CodexApprovalPolicy> {
    match value.trim().to_ascii_lowercase().as_str() {
        "untrusted" => Some(CodexApprovalPolicy::Untrusted),
        "on-failure" => Some(CodexApprovalPolicy::OnFailure),
        "on-request" => Some(CodexApprovalPolicy::OnRequest),
        "never" => Some(CodexApprovalPolicy::Never),
        _ => None,
    }
}

fn parse_discovered_codex_reasoning_effort(value: &str) -> Option<CodexReasoningEffort> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(CodexReasoningEffort::None),
        "minimal" => Some(CodexReasoningEffort::Minimal),
        "low" => Some(CodexReasoningEffort::Low),
        "medium" => Some(CodexReasoningEffort::Medium),
        "high" => Some(CodexReasoningEffort::High),
        "xhigh" => Some(CodexReasoningEffort::XHigh),
        _ => None,
    }
}

fn codex_discovery_scope_contains(root_path: &str, candidate_path: &FsPath) -> bool {
    let root = normalize_codex_discovery_path(FsPath::new(root_path));
    let candidate = normalize_codex_discovery_path(candidate_path);
    candidate == root || candidate.starts_with(root)
}

fn normalize_codex_discovery_path(path: &FsPath) -> PathBuf {
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    fs::canonicalize(&resolved).unwrap_or(resolved)
}

fn apply_discovered_codex_thread(
    record: &mut SessionRecord,
    thread: &DiscoveredCodexThread,
    overwrite_prompt_settings: bool,
) {
    set_record_external_session_id(record, Some(thread.id.clone()));
    set_record_codex_thread_state(
        record,
        if thread.archived {
            CodexThreadState::Archived
        } else {
            CodexThreadState::Active
        },
    );

    if overwrite_prompt_settings {
        if let Some(model) = thread.model.as_ref() {
            record.session.model = model.clone();
        }
        if let Some(sandbox_mode) = thread.sandbox_mode {
            record.codex_sandbox_mode = sandbox_mode;
            record.session.sandbox_mode = Some(sandbox_mode);
        }
        if let Some(approval_policy) = thread.approval_policy {
            record.codex_approval_policy = approval_policy;
            record.session.approval_policy = Some(approval_policy);
        }
        if let Some(reasoning_effort) = thread.reasoning_effort {
            record.codex_reasoning_effort = reasoning_effort;
            record.session.reasoning_effort = Some(reasoning_effort);
        }
    }

    if record.session.messages.is_empty() && matches!(record.session.status, SessionStatus::Idle) {
        record.session.preview = if thread.archived {
            "Archived Codex thread ready to reopen.".to_owned()
        } else {
            "Ready to continue this Codex thread.".to_owned()
        };
    }
}

fn normalize_remote_configs(remotes: Vec<RemoteConfig>) -> Result<Vec<RemoteConfig>, ApiError> {
    let mut normalized = vec![RemoteConfig::local()];
    let mut seen_ids = HashSet::from([default_local_remote_id()]);

    for remote in remotes {
        let id = remote.id.trim();
        validate_remote_id_value(id)?;
        if id.eq_ignore_ascii_case(LOCAL_REMOTE_ID) {
            continue;
        }
        if !seen_ids.insert(id.to_owned()) {
            return Err(ApiError::bad_request(format!("duplicate remote id `{id}`")));
        }

        let name = remote.name.trim();
        if name.is_empty() {
            return Err(ApiError::bad_request(format!(
                "remote `{id}` must have a name"
            )));
        }

        match remote.transport {
            RemoteTransport::Local => {
                return Err(ApiError::bad_request(format!(
                    "remote `{id}` cannot use local transport"
                )));
            }
            RemoteTransport::Ssh => {
                let host = normalized_remote_ssh_host(&remote)?;
                normalized.push(RemoteConfig {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    transport: RemoteTransport::Ssh,
                    enabled: remote.enabled,
                    host: Some(host),
                    port: Some(remote.port.unwrap_or(DEFAULT_SSH_REMOTE_PORT)),
                    user: normalized_remote_ssh_user(&remote)?,
                });
            }
        }
    }

    Ok(normalized)
}

fn normalize_persisted_remote_configs(remotes: Vec<RemoteConfig>) -> Vec<RemoteConfig> {
    normalize_remote_configs(remotes).unwrap_or_else(|_| default_remote_configs())
}

struct StateInner {
    codex: CodexState,
    preferences: AppPreferences,
    revision: u64,
    next_project_number: usize,
    next_session_number: usize,
    next_message_number: u64,
    projects: Vec<Project>,
    ignored_discovered_codex_thread_ids: BTreeSet<String>,
    sessions: Vec<SessionRecord>,
}

impl StateInner {
    fn new() -> Self {
        Self {
            codex: CodexState::default(),
            preferences: AppPreferences::default(),
            revision: 0,
            next_project_number: 1,
            next_session_number: 1,
            next_message_number: 1,
            projects: Vec::new(),
            ignored_discovered_codex_thread_ids: BTreeSet::new(),
            sessions: Vec::new(),
        }
    }

    fn create_project(
        &mut self,
        name: Option<String>,
        root_path: String,
        remote_id: String,
    ) -> Project {
        if let Some(existing) = self
            .projects
            .iter()
            .find(|project| project.remote_id == remote_id && project.root_path == root_path)
            .cloned()
        {
            return existing;
        }

        let number = self.next_project_number;
        self.next_project_number += 1;
        let base_name = name.unwrap_or_else(|| default_project_name(&root_path));
        let project = Project {
            id: format!("project-{number}"),
            name: dedupe_project_name(&self.projects, &base_name),
            root_path,
            remote_id,
            remote_project_id: None,
        };
        self.projects.push(project.clone());
        project
    }

    fn create_session(
        &mut self,
        agent: Agent,
        name: Option<String>,
        workdir: String,
        project_id: Option<String>,
        model: Option<String>,
    ) -> SessionRecord {
        let number = self.next_session_number;
        self.next_session_number += 1;

        let record = SessionRecord {
            active_codex_approval_policy: None,
            active_codex_reasoning_effort: None,
            active_codex_sandbox_mode: None,
            agent_commands: Vec::new(),
            codex_approval_policy: default_codex_approval_policy(),
            codex_reasoning_effort: self.preferences.default_codex_reasoning_effort,
            codex_sandbox_mode: default_codex_sandbox_mode(),
            external_session_id: None,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_codex_user_inputs: HashMap::new(),
            pending_codex_mcp_elicitations: HashMap::new(),
            pending_codex_app_requests: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            queued_prompts: VecDeque::new(),
            message_positions: HashMap::new(),
            remote_id: None,
            remote_session_id: None,
            runtime: SessionRuntime::None,
            runtime_reset_required: false,
            hidden: false,
            session: Session {
                id: format!("session-{number}"),
                name: name.unwrap_or_else(|| format!("{} {}", agent.name(), number)),
                emoji: agent.avatar().to_owned(),
                agent,
                workdir,
                project_id,
                model: model.unwrap_or_else(|| agent.default_model().to_owned()),
                model_options: Vec::new(),
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: agent
                    .supports_cursor_mode()
                    .then_some(default_cursor_mode()),
                claude_approval_mode: agent
                    .supports_claude_approval_mode()
                    .then_some(default_claude_approval_mode()),
                claude_effort: agent
                    .supports_claude_approval_mode()
                    .then_some(self.preferences.default_claude_effort),
                gemini_approval_mode: agent
                    .supports_gemini_approval_mode()
                    .then_some(default_gemini_approval_mode()),
                external_session_id: None,
                agent_commands_revision: 0,
                codex_thread_state: None,
                status: SessionStatus::Idle,
                preview: "Ready for a prompt.".to_owned(),
                messages: Vec::new(),
                pending_prompts: Vec::new(),
            },
        };

        let mut record = record;
        if record.session.agent.supports_codex_prompt_settings() {
            record.session.approval_policy = Some(record.codex_approval_policy);
            record.session.reasoning_effort = Some(record.codex_reasoning_effort);
            record.session.sandbox_mode = Some(record.codex_sandbox_mode);
        } else if record.session.agent.supports_claude_approval_mode() {
            record.session.claude_effort = Some(self.preferences.default_claude_effort);
        }

        self.sessions.push(record.clone());
        record
    }

    fn ignore_discovered_codex_thread(&mut self, thread_id: Option<&str>) {
        if let Some(thread_id) = normalize_optional_identifier(thread_id) {
            self.ignored_discovered_codex_thread_ids
                .insert(thread_id.to_owned());
        }
    }

    fn allow_discovered_codex_thread(&mut self, thread_id: Option<&str>) {
        if let Some(thread_id) = normalize_optional_identifier(thread_id) {
            self.ignored_discovered_codex_thread_ids.remove(thread_id);
        }
    }

    fn find_matching_hidden_claude_spare(
        &self,
        workdir: &str,
        project_id: Option<&str>,
        model: &str,
        approval_mode: ClaudeApprovalMode,
        effort: ClaudeEffortLevel,
    ) -> Option<usize> {
        self.sessions.iter().position(|record| {
            record.hidden
                && !record.is_remote_proxy()
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
                && record.session.project_id.as_deref() == project_id
                && record.session.model == model
                && record.session.claude_approval_mode == Some(approval_mode)
                && record.session.claude_effort == Some(effort)
        })
    }

    fn ensure_hidden_claude_spare(
        &mut self,
        workdir: String,
        project_id: Option<String>,
        model: String,
        approval_mode: ClaudeApprovalMode,
        effort: ClaudeEffortLevel,
    ) -> Option<String> {
        if let Some(index) = self.find_matching_hidden_claude_spare(
            &workdir,
            project_id.as_deref(),
            &model,
            approval_mode,
            effort,
        ) {
            let record = &mut self.sessions[index];
            reset_hidden_claude_spare_record(record);
            return matches!(record.runtime, SessionRuntime::None)
                .then(|| record.session.id.clone());
        }

        self.create_session(Agent::Claude, None, workdir, project_id, Some(model));
        let record = self
            .sessions
            .last_mut()
            .expect("create_session should append a session record");
        record.hidden = true;
        record.session.claude_approval_mode = Some(approval_mode);
        record.session.claude_effort = Some(effort);
        reset_hidden_claude_spare_record(record);
        Some(record.session.id.clone())
    }

    fn next_message_id(&mut self) -> String {
        let id = format!("message-{}", self.next_message_number);
        self.next_message_number += 1;
        id
    }

    fn find_session_index(&self, session_id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|record| record.session.id == session_id)
    }

    fn find_visible_session_index(&self, session_id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|record| !record.hidden && record.session.id == session_id)
    }

    fn find_remote_session_index(&self, remote_id: &str, remote_session_id: &str) -> Option<usize> {
        self.sessions.iter().position(|record| {
            record.remote_id.as_deref() == Some(remote_id)
                && record.remote_session_id.as_deref() == Some(remote_session_id)
        })
    }

    fn find_project(&self, project_id: &str) -> Option<&Project> {
        self.projects
            .iter()
            .find(|project| project.id == project_id)
    }

    fn find_remote(&self, remote_id: &str) -> Option<&RemoteConfig> {
        self.preferences
            .remotes
            .iter()
            .find(|remote| remote.id == remote_id)
    }

    fn find_project_for_workdir(&self, workdir: &str) -> Option<&Project> {
        let target = FsPath::new(workdir);
        self.projects
            .iter()
            .filter(|project| {
                project.remote_id == LOCAL_REMOTE_ID
                    && codex_discovery_scope_contains(&project.root_path, target)
            })
            .max_by_key(|project| project.root_path.len())
    }

    fn import_discovered_codex_threads(
        &mut self,
        default_workdir: &str,
        threads: Vec<DiscoveredCodexThread>,
    ) {
        let discovered_thread_ids = threads
            .iter()
            .filter_map(|thread| normalize_optional_identifier(Some(thread.id.as_str())))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        self.ignored_discovered_codex_thread_ids
            .retain(|thread_id| discovered_thread_ids.contains(thread_id));

        for thread in threads {
            let target_path = FsPath::new(&thread.cwd);
            let within_scope = codex_discovery_scope_contains(default_workdir, target_path)
                || self.projects.iter().any(|project| {
                    project.remote_id == LOCAL_REMOTE_ID
                        && codex_discovery_scope_contains(&project.root_path, target_path)
                });
            if !within_scope {
                continue;
            }

            let project_id = self
                .find_project_for_workdir(&thread.cwd)
                .map(|project| project.id.clone())
                .unwrap_or_else(|| {
                    self.create_project(None, thread.cwd.clone(), default_local_remote_id())
                        .id
                });

            let existing_index = self.sessions.iter().position(|record| {
                !record.is_remote_proxy()
                    && record.session.agent == Agent::Codex
                    && record.external_session_id.as_deref() == Some(thread.id.as_str())
            });

            if let Some(index) = existing_index {
                self.allow_discovered_codex_thread(Some(thread.id.as_str()));
                let record = &mut self.sessions[index];
                if record.session.workdir != thread.cwd {
                    record.session.workdir = thread.cwd.clone();
                }
                if record.session.project_id.as_deref() != Some(project_id.as_str()) {
                    record.session.project_id = Some(project_id);
                }
                apply_discovered_codex_thread(record, &thread, false);
                continue;
            }

            if self
                .ignored_discovered_codex_thread_ids
                .contains(thread.id.as_str())
            {
                continue;
            }

            let mut record = self.create_session(
                Agent::Codex,
                Some(thread.title.clone()),
                thread.cwd.clone(),
                Some(project_id),
                thread.model.clone(),
            );
            apply_discovered_codex_thread(&mut record, &thread, true);
            if let Some(slot) = self
                .find_session_index(&record.session.id)
                .and_then(|index| self.sessions.get_mut(index))
            {
                *slot = record;
            }
        }
    }

    fn ensure_projects_consistent(&mut self) {
        for project in &mut self.projects {
            if project.remote_id.trim().is_empty() {
                project.remote_id = default_local_remote_id();
            }
        }

        let highest_project_number = self
            .projects
            .iter()
            .filter_map(|project| {
                project
                    .id
                    .strip_prefix("project-")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .max()
            .unwrap_or(0);
        self.next_project_number = self
            .next_project_number
            .max(highest_project_number.saturating_add(1))
            .max(1);

        for index in 0..self.sessions.len() {
            let existing_project_id = self.sessions[index].session.project_id.clone();
            if existing_project_id
                .as_deref()
                .and_then(|project_id| self.find_project(project_id))
                .is_some()
            {
                continue;
            }

            let workdir = self.sessions[index].session.workdir.clone();
            let project_id = self
                .find_project_for_workdir(&workdir)
                .map(|project| project.id.clone())
                .unwrap_or_else(|| {
                    self.create_project(None, workdir, default_local_remote_id())
                        .id
                });
            self.sessions[index].session.project_id = Some(project_id);
        }
    }

    fn recover_interrupted_sessions(&mut self) {
        for index in 0..self.sessions.len() {
            if self.sessions[index].is_remote_proxy() {
                continue;
            }
            let recovery = {
                let record = &mut self.sessions[index];
                recover_interrupted_session_record(record)
            };

            let Some(recovery) = recovery else {
                continue;
            };

            let message_id = self.next_message_id();
            let record = &mut self.sessions[index];
            push_message_on_record(
                record,
                Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: recovery,
                    expanded_text: None,
                },
            );
            record.session.status = SessionStatus::Error;
            if let Some(message) = record.session.messages.last() {
                if let Some(preview) = message.preview_text() {
                    record.session.preview = preview;
                }
            }
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    #[serde(default, skip_serializing_if = "CodexState::is_empty")]
    codex: CodexState,
    #[serde(default)]
    preferences: AppPreferences,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    next_project_number: usize,
    next_session_number: usize,
    next_message_number: u64,
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    ignored_discovered_codex_thread_ids: BTreeSet<String>,
    sessions: Vec<PersistedSessionRecord>,
}

impl PersistedState {
    fn from_inner(inner: &StateInner) -> Self {
        Self {
            codex: inner.codex.clone(),
            preferences: inner.preferences.clone(),
            revision: inner.revision,
            next_project_number: inner.next_project_number,
            next_session_number: inner.next_session_number,
            next_message_number: inner.next_message_number,
            projects: inner.projects.clone(),
            ignored_discovered_codex_thread_ids: inner
                .ignored_discovered_codex_thread_ids
                .clone(),
            sessions: inner
                .sessions
                .iter()
                .filter(|record| !record.hidden)
                .map(PersistedSessionRecord::from_record)
                .collect(),
        }
    }

    fn into_inner(self) -> StateInner {
        let mut inner = StateInner {
            codex: self.codex,
            preferences: AppPreferences {
                remotes: normalize_persisted_remote_configs(self.preferences.remotes),
                ..self.preferences
            },
            revision: self.revision,
            next_project_number: self.next_project_number.max(1),
            next_session_number: self.next_session_number,
            next_message_number: self.next_message_number,
            projects: self.projects,
            ignored_discovered_codex_thread_ids: self.ignored_discovered_codex_thread_ids,
            sessions: self
                .sessions
                .into_iter()
                .map(PersistedSessionRecord::into_record)
                .collect(),
        };
        inner.ensure_projects_consistent();
        inner.recover_interrupted_sessions();
        inner
    }
}

fn same_codex_notice_identity(left: &CodexNotice, right: &CodexNotice) -> bool {
    left.kind == right.kind
        && left.level == right.level
        && left.title == right.title
        && left.detail == right.detail
        && left.code == right.code
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedSessionRecord {
    active_codex_approval_policy: Option<CodexApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_codex_reasoning_effort: Option<CodexReasoningEffort>,
    active_codex_sandbox_mode: Option<CodexSandboxMode>,
    codex_approval_policy: CodexApprovalPolicy,
    #[serde(default = "default_codex_reasoning_effort")]
    codex_reasoning_effort: CodexReasoningEffort,
    codex_sandbox_mode: CodexSandboxMode,
    external_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    queued_prompts: VecDeque<QueuedPromptRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_session_id: Option<String>,
    session: Session,
}

impl PersistedSessionRecord {
    fn from_record(record: &SessionRecord) -> Self {
        let mut session = record.session.clone();
        if !record.is_remote_proxy() {
            session.pending_prompts.clear();
        }

        Self {
            active_codex_approval_policy: record.active_codex_approval_policy,
            active_codex_reasoning_effort: record.active_codex_reasoning_effort,
            active_codex_sandbox_mode: record.active_codex_sandbox_mode,
            codex_approval_policy: record.codex_approval_policy,
            codex_reasoning_effort: record.codex_reasoning_effort,
            codex_sandbox_mode: record.codex_sandbox_mode,
            external_session_id: record.external_session_id.clone(),
            queued_prompts: record.queued_prompts.clone(),
            remote_id: record.remote_id.clone(),
            remote_session_id: record.remote_session_id.clone(),
            session,
        }
    }

    fn into_record(self) -> SessionRecord {
        let mut session = self.session;
        session.external_session_id = self.external_session_id.clone();
        if session.agent.acp_runtime().is_none() {
            session.model_options.clear();
        }
        if session.agent.supports_cursor_mode() {
            session.cursor_mode.get_or_insert_with(default_cursor_mode);
        } else {
            session.cursor_mode = None;
        }
        if session.agent.supports_claude_approval_mode() {
            session
                .claude_approval_mode
                .get_or_insert_with(default_claude_approval_mode);
            session
                .claude_effort
                .get_or_insert_with(default_claude_effort);
        } else {
            session.claude_approval_mode = None;
            session.claude_effort = None;
        }
        if session.agent.supports_gemini_approval_mode() {
            session
                .gemini_approval_mode
                .get_or_insert_with(default_gemini_approval_mode);
        } else {
            session.gemini_approval_mode = None;
        }
        if session.agent.supports_codex_prompt_settings() {
            session
                .reasoning_effort
                .get_or_insert_with(default_codex_reasoning_effort);
        } else {
            session.reasoning_effort = None;
        }
        if self.remote_id.is_none() {
            session.pending_prompts.clear();
        }

        let mut record = SessionRecord {
            active_codex_approval_policy: self.active_codex_approval_policy,
            active_codex_reasoning_effort: self.active_codex_reasoning_effort,
            active_codex_sandbox_mode: self.active_codex_sandbox_mode,
            agent_commands: Vec::new(),
            codex_approval_policy: self.codex_approval_policy,
            codex_reasoning_effort: self.codex_reasoning_effort,
            codex_sandbox_mode: self.codex_sandbox_mode,
            external_session_id: self.external_session_id,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_codex_user_inputs: HashMap::new(),
            pending_codex_mcp_elicitations: HashMap::new(),
            pending_codex_app_requests: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            queued_prompts: self.queued_prompts,
            message_positions: build_message_positions(&session.messages),
            remote_id: self.remote_id,
            remote_session_id: self.remote_session_id,
            runtime: SessionRuntime::None,
            runtime_reset_required: false,
            hidden: false,
            session,
        };
        sync_codex_thread_state(&mut record);
        sync_pending_prompts(&mut record);
        record
    }
}

#[derive(Clone)]
struct SessionRecord {
    active_codex_approval_policy: Option<CodexApprovalPolicy>,
    active_codex_reasoning_effort: Option<CodexReasoningEffort>,
    active_codex_sandbox_mode: Option<CodexSandboxMode>,
    agent_commands: Vec<AgentCommand>,
    codex_approval_policy: CodexApprovalPolicy,
    codex_reasoning_effort: CodexReasoningEffort,
    codex_sandbox_mode: CodexSandboxMode,
    external_session_id: Option<String>,
    pending_claude_approvals: HashMap<String, ClaudePendingApproval>,
    pending_codex_approvals: HashMap<String, CodexPendingApproval>,
    pending_codex_user_inputs: HashMap<String, CodexPendingUserInput>,
    pending_codex_mcp_elicitations: HashMap<String, CodexPendingMcpElicitation>,
    pending_codex_app_requests: HashMap<String, CodexPendingAppRequest>,
    pending_acp_approvals: HashMap<String, AcpPendingApproval>,
    queued_prompts: VecDeque<QueuedPromptRecord>,
    message_positions: HashMap<String, usize>,
    remote_id: Option<String>,
    remote_session_id: Option<String>,
    runtime: SessionRuntime,
    runtime_reset_required: bool,
    hidden: bool,
    session: Session,
}

impl SessionRecord {
    fn is_remote_proxy(&self) -> bool {
        self.remote_id.is_some() && self.remote_session_id.is_some()
    }
}

fn reset_hidden_claude_spare_record(record: &mut SessionRecord) {
    if record.session.agent != Agent::Claude {
        return;
    }

    record.session.messages.clear();
    record.session.pending_prompts.clear();
    record.session.status = SessionStatus::Idle;
    record.session.preview = "Ready for a prompt.".to_owned();
    clear_all_pending_requests(record);
    record.queued_prompts.clear();
    record.message_positions.clear();
    record.runtime_reset_required = false;
}

fn has_pending_requests(record: &SessionRecord) -> bool {
    !record.pending_claude_approvals.is_empty()
        || !record.pending_codex_approvals.is_empty()
        || !record.pending_codex_user_inputs.is_empty()
        || !record.pending_codex_mcp_elicitations.is_empty()
        || !record.pending_codex_app_requests.is_empty()
        || !record.pending_acp_approvals.is_empty()
}

fn clear_all_pending_requests(record: &mut SessionRecord) {
    record.pending_claude_approvals.clear();
    record.pending_codex_approvals.clear();
    record.pending_codex_user_inputs.clear();
    record.pending_codex_mcp_elicitations.clear();
    record.pending_codex_app_requests.clear();
    record.pending_acp_approvals.clear();
}

fn merge_agent_commands(
    preferred: &[AgentCommand],
    fallback: &[AgentCommand],
) -> Vec<AgentCommand> {
    if preferred.is_empty() {
        return dedupe_agent_commands(fallback.to_vec());
    }
    if fallback.is_empty() {
        return dedupe_agent_commands(preferred.to_vec());
    }

    let mut commands = preferred.to_vec();
    commands.extend(fallback.iter().cloned());
    dedupe_agent_commands(commands)
}

fn dedupe_agent_commands(commands: Vec<AgentCommand>) -> Vec<AgentCommand> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for command in commands {
        let key = command.name.trim().to_ascii_lowercase();
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        deduped.push(command);
    }
    deduped.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    deduped
}

fn claude_spare_profile(
    record: &SessionRecord,
) -> (String, Option<String>, String, ClaudeApprovalMode, ClaudeEffortLevel) {
    (
        record.session.workdir.clone(),
        record.session.project_id.clone(),
        record.session.model.clone(),
        record
            .session
            .claude_approval_mode
            .unwrap_or_else(default_claude_approval_mode),
        record
            .session
            .claude_effort
            .unwrap_or_else(default_claude_effort),
    )
}

fn normalized_codex_thread_state(
    agent: Agent,
    external_session_id: Option<&str>,
    current_state: Option<CodexThreadState>,
) -> Option<CodexThreadState> {
    if !agent.supports_codex_prompt_settings() || external_session_id.is_none() {
        return None;
    }

    Some(current_state.unwrap_or(CodexThreadState::Active))
}

fn sync_codex_thread_state(record: &mut SessionRecord) {
    record.session.codex_thread_state = normalized_codex_thread_state(
        record.session.agent,
        record.external_session_id.as_deref(),
        record.session.codex_thread_state,
    );
}

fn set_record_external_session_id(record: &mut SessionRecord, external_session_id: Option<String>) {
    record.external_session_id = external_session_id.clone();
    record.session.external_session_id = external_session_id;
    sync_codex_thread_state(record);
}

fn set_record_codex_thread_state(record: &mut SessionRecord, thread_state: CodexThreadState) {
    record.session.codex_thread_state = normalized_codex_thread_state(
        record.session.agent,
        record.external_session_id.as_deref(),
        Some(thread_state),
    );
}

fn record_has_archived_codex_thread(record: &SessionRecord) -> bool {
    record.session.codex_thread_state == Some(CodexThreadState::Archived)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct QueuedPromptRecord {
    attachments: Vec<PromptImageAttachment>,
    pending_prompt: PendingPrompt,
}

fn sync_pending_prompts(record: &mut SessionRecord) {
    if record.is_remote_proxy() {
        return;
    }
    record.session.pending_prompts = record
        .queued_prompts
        .iter()
        .map(|queued| queued.pending_prompt.clone())
        .collect();
}

fn set_approval_decision_on_record(
    record: &mut SessionRecord,
    message_id: &str,
    decision: ApprovalDecision,
) -> Result<()> {
    let Some(message_index) = message_index_on_record(record, message_id) else {
        return Err(anyhow!("approval message `{message_id}` not found"));
    };
    let Some(message) = record.session.messages.get_mut(message_index) else {
        return Err(anyhow!("approval message `{message_id}` not found"));
    };
    match message {
        Message::Approval {
            id,
            decision: current,
            ..
        } if id == message_id => {
            *current = decision;
            Ok(())
        }
        _ => Err(anyhow!("approval message `{message_id}` not found")),
    }
}

fn set_user_input_request_state_on_record(
    record: &mut SessionRecord,
    message_id: &str,
    state: InteractionRequestState,
    submitted_answers: Option<BTreeMap<String, Vec<String>>>,
) -> Result<()> {
    let Some(message_index) = message_index_on_record(record, message_id) else {
        return Err(anyhow!("user input request `{message_id}` not found"));
    };
    let Some(message) = record.session.messages.get_mut(message_index) else {
        return Err(anyhow!("user input request `{message_id}` not found"));
    };
    match message {
        Message::UserInputRequest {
            id,
            state: current_state,
            submitted_answers: current_answers,
            ..
        } if id == message_id => {
            *current_state = state;
            *current_answers = submitted_answers;
            Ok(())
        }
        _ => Err(anyhow!("user input request `{message_id}` not found")),
    }
}

fn set_mcp_elicitation_request_state_on_record(
    record: &mut SessionRecord,
    message_id: &str,
    state: InteractionRequestState,
    submitted_action: Option<McpElicitationAction>,
    submitted_content: Option<Value>,
) -> Result<()> {
    let Some(message_index) = message_index_on_record(record, message_id) else {
        return Err(anyhow!("MCP elicitation request `{message_id}` not found"));
    };
    let Some(message) = record.session.messages.get_mut(message_index) else {
        return Err(anyhow!("MCP elicitation request `{message_id}` not found"));
    };
    match message {
        Message::McpElicitationRequest {
            id,
            state: current_state,
            submitted_action: current_action,
            submitted_content: current_content,
            ..
        } if id == message_id => {
            *current_state = state;
            *current_action = submitted_action;
            *current_content = submitted_content;
            Ok(())
        }
        _ => Err(anyhow!("MCP elicitation request `{message_id}` not found")),
    }
}

fn set_codex_app_request_state_on_record(
    record: &mut SessionRecord,
    message_id: &str,
    state: InteractionRequestState,
    submitted_result: Option<Value>,
) -> Result<()> {
    let Some(message_index) = message_index_on_record(record, message_id) else {
        return Err(anyhow!("Codex app request `{message_id}` not found"));
    };
    let Some(message) = record.session.messages.get_mut(message_index) else {
        return Err(anyhow!("Codex app request `{message_id}` not found"));
    };
    match message {
        Message::CodexAppRequest {
            id,
            state: current_state,
            submitted_result: current_result,
            ..
        } if id == message_id => {
            *current_state = state;
            *current_result = submitted_result;
            Ok(())
        }
        _ => Err(anyhow!("Codex app request `{message_id}` not found")),
    }
}

fn latest_pending_interaction_preview(record: &SessionRecord) -> Option<String> {
    for message in record.session.messages.iter().rev() {
        match message {
            Message::Approval {
                decision: ApprovalDecision::Pending,
                ..
            } => {
                return Some(approval_preview_text(
                    record.session.agent.name(),
                    ApprovalDecision::Pending,
                ));
            }
            Message::UserInputRequest {
                state: InteractionRequestState::Pending,
                ..
            } => {
                return Some(user_input_request_preview_text(
                    record.session.agent.name(),
                    InteractionRequestState::Pending,
                ));
            }
            Message::McpElicitationRequest {
                state: InteractionRequestState::Pending,
                ..
            } => {
                return Some(mcp_elicitation_request_preview_text(
                    record.session.agent.name(),
                    InteractionRequestState::Pending,
                    None,
                ));
            }
            Message::CodexAppRequest {
                state: InteractionRequestState::Pending,
                ..
            } => {
                return Some(codex_app_request_preview_text(
                    record.session.agent.name(),
                    InteractionRequestState::Pending,
                ));
            }
            _ => {}
        }
    }

    None
}

fn approval_preview_text(agent_name: &str, decision: ApprovalDecision) -> String {
    match decision {
        ApprovalDecision::Pending => "Approval pending.".to_owned(),
        ApprovalDecision::Interrupted => "Approval expired after TermAl restarted.".to_owned(),
        ApprovalDecision::Canceled => {
            format!("Approval canceled. {agent_name} is continuing\u{2026}")
        }
        ApprovalDecision::Accepted => {
            format!("Approval granted. {agent_name} is continuing\u{2026}")
        }
        ApprovalDecision::AcceptedForSession => {
            format!("Approval granted for this session. {agent_name} is continuing\u{2026}")
        }
        ApprovalDecision::Rejected => {
            format!("Approval rejected. {agent_name} is continuing\u{2026}")
        }
    }
}

fn user_input_request_preview_text(
    agent_name: &str,
    state: InteractionRequestState,
) -> String {
    match state {
        InteractionRequestState::Pending => "Input requested.".to_owned(),
        InteractionRequestState::Submitted => {
            format!("Input submitted. {agent_name} is continuing\u{2026}")
        }
        InteractionRequestState::Interrupted => {
            "Input request expired after TermAl restarted.".to_owned()
        }
        InteractionRequestState::Canceled => {
            format!("Input request canceled. {agent_name} is continuing\u{2026}")
        }
    }
}

fn mcp_elicitation_request_preview_text(
    agent_name: &str,
    state: InteractionRequestState,
    action: Option<McpElicitationAction>,
) -> String {
    match state {
        InteractionRequestState::Pending => "MCP input requested.".to_owned(),
        InteractionRequestState::Submitted => match action.unwrap_or(McpElicitationAction::Accept) {
            McpElicitationAction::Accept => {
                format!("MCP input submitted. {agent_name} is continuing\u{2026}")
            }
            McpElicitationAction::Decline => {
                format!("MCP request declined. {agent_name} is continuing\u{2026}")
            }
            McpElicitationAction::Cancel => {
                format!("MCP request canceled. {agent_name} is continuing\u{2026}")
            }
        },
        InteractionRequestState::Interrupted => {
            "MCP input request expired after TermAl restarted.".to_owned()
        }
        InteractionRequestState::Canceled => {
            format!("MCP input request canceled. {agent_name} is continuing\u{2026}")
        }
    }
}

fn codex_app_request_preview_text(agent_name: &str, state: InteractionRequestState) -> String {
    match state {
        InteractionRequestState::Pending => "Codex response requested.".to_owned(),
        InteractionRequestState::Submitted => {
            format!("Codex response submitted. {agent_name} is continuing\u{2026}")
        }
        InteractionRequestState::Interrupted => {
            "Codex request expired after TermAl restarted.".to_owned()
        }
        InteractionRequestState::Canceled => {
            format!("Codex request canceled. {agent_name} is continuing\u{2026}")
        }
    }
}

fn sync_session_interaction_state(record: &mut SessionRecord, resolved_preview: String) {
    if let Some(preview) = latest_pending_interaction_preview(record) {
        record.session.status = SessionStatus::Approval;
        record.session.preview = preview;
        return;
    }

    if matches!(
        record.session.status,
        SessionStatus::Approval | SessionStatus::Active
    ) {
        record.session.status = SessionStatus::Active;
        record.session.preview = resolved_preview;
    }
}

fn validate_codex_user_input_answers(
    questions: &[UserInputQuestion],
    answers: BTreeMap<String, Vec<String>>,
) -> std::result::Result<
    (
        BTreeMap<String, BTreeMap<String, Vec<String>>>,
        BTreeMap<String, Vec<String>>,
    ),
    ApiError,
> {
    if questions.is_empty() {
        return Err(ApiError::bad_request(
            "Codex did not include any questions for this request",
        ));
    }

    let question_ids: HashSet<&str> = questions
        .iter()
        .map(|question| question.id.as_str())
        .collect();
    for answer_id in answers.keys() {
        if !question_ids.contains(answer_id.as_str()) {
            return Err(ApiError::bad_request(format!(
                "answer `{answer_id}` does not match any requested question"
            )));
        }
    }

    let mut response_answers = BTreeMap::new();
    let mut display_answers = BTreeMap::new();
    for question in questions {
        let Some(raw_answers) = answers.get(&question.id) else {
            return Err(ApiError::bad_request(format!(
                "question `{}` is missing an answer",
                question.header
            )));
        };

        let normalized_answers = raw_answers
            .iter()
            .map(|answer: &String| answer.trim())
            .filter(|answer: &&str| !answer.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if normalized_answers.len() != 1 {
            return Err(ApiError::bad_request(format!(
                "question `{}` requires exactly one answer",
                question.header
            )));
        }

        if let Some(options) = question.options.as_ref() {
            let selected = &normalized_answers[0];
            let matches_option = options.iter().any(|option| option.label == *selected);
            if !matches_option && !question.is_other {
                return Err(ApiError::bad_request(format!(
                    "question `{}` must use one of the provided options",
                    question.header
                )));
            }
        }

        response_answers.insert(
            question.id.clone(),
            BTreeMap::from([("answers".to_owned(), normalized_answers.clone())]),
        );
        display_answers.insert(
            question.id.clone(),
            if question.is_secret {
                vec!["[secret provided]".to_owned()]
            } else {
                normalized_answers
            },
        );
    }

    Ok((response_answers, display_answers))
}

fn validate_codex_mcp_elicitation_submission(
    request: &McpElicitationRequestPayload,
    action: McpElicitationAction,
    content: Option<Value>,
) -> std::result::Result<Option<Value>, ApiError> {
    let content = content.filter(|value| !value.is_null());
    match (&request.mode, action) {
        (McpElicitationRequestMode::Url { .. }, _) => {
            if content.is_some() {
                return Err(ApiError::bad_request(
                    "URL-based MCP elicitations do not accept structured content",
                ));
            }
            Ok(None)
        }
        (McpElicitationRequestMode::Form { .. }, McpElicitationAction::Accept) => {
            let content = content.ok_or_else(|| {
                ApiError::bad_request(
                    "accepted MCP elicitation responses must include structured content",
                )
            })?;
            Ok(Some(validate_codex_mcp_elicitation_form_content(
                request, content,
            )?))
        }
        (McpElicitationRequestMode::Form { .. }, _) => {
            if content.is_some() {
                return Err(ApiError::bad_request(
                    "declined or canceled MCP elicitations cannot include structured content",
                ));
            }
            Ok(None)
        }
    }
}

fn validate_codex_mcp_elicitation_form_content(
    request: &McpElicitationRequestPayload,
    content: Value,
) -> std::result::Result<Value, ApiError> {
    let McpElicitationRequestMode::Form {
        requested_schema, ..
    } = &request.mode
    else {
        return Err(ApiError::bad_request(
            "structured content is only supported for form-mode MCP elicitations",
        ));
    };

    if requested_schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(ApiError::bad_request(
            "MCP elicitation schema must be an object schema",
        ));
    }
    let properties = requested_schema
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            ApiError::bad_request("MCP elicitation schema is missing form properties")
        })?;
    let required = requested_schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let object = content.as_object().ok_or_else(|| {
        ApiError::bad_request("MCP elicitation content must be a JSON object")
    })?;

    for key in object.keys() {
        if !properties.contains_key(key) {
            return Err(ApiError::bad_request(format!(
                "field `{key}` is not part of this MCP elicitation",
            )));
        }
    }
    for required_key in required.iter().filter_map(Value::as_str) {
        if !object.contains_key(required_key) {
            return Err(ApiError::bad_request(format!(
                "field `{required_key}` is required for this MCP elicitation",
            )));
        }
    }

    let mut normalized = serde_json::Map::new();
    for (key, value) in object {
        let schema = properties.get(key).ok_or_else(|| {
            ApiError::bad_request(format!(
                "field `{key}` is not part of this MCP elicitation",
            ))
        })?;
        normalized.insert(
            key.clone(),
            validate_codex_mcp_elicitation_field_value(key, schema, value)?,
        );
    }

    Ok(Value::Object(normalized))
}

fn validate_codex_mcp_elicitation_field_value(
    field_name: &str,
    schema: &Value,
    value: &Value,
) -> std::result::Result<Value, ApiError> {
    match schema.get("type").and_then(Value::as_str) {
        Some("boolean") => {
            if !value.is_boolean() {
                return Err(ApiError::bad_request(format!(
                    "field `{field_name}` must be true or false",
                )));
            }
            Ok(value.clone())
        }
        Some("number") => {
            validate_codex_mcp_elicitation_number_value(field_name, schema, value, false)
        }
        Some("integer") => {
            validate_codex_mcp_elicitation_number_value(field_name, schema, value, true)
        }
        Some("string") => validate_codex_mcp_elicitation_string_value(field_name, schema, value),
        Some("array") => validate_codex_mcp_elicitation_array_value(field_name, schema, value),
        Some(other) => Err(ApiError::bad_request(format!(
            "field `{field_name}` uses unsupported MCP elicitation type `{other}`",
        ))),
        None => Err(ApiError::bad_request(format!(
            "field `{field_name}` is missing an MCP elicitation type",
        ))),
    }
}

fn validate_codex_mcp_elicitation_number_value(
    field_name: &str,
    schema: &Value,
    value: &Value,
    require_integer: bool,
) -> std::result::Result<Value, ApiError> {
    let Some(number) = value.as_f64() else {
        let expected = if require_integer { "an integer" } else { "a number" };
        return Err(ApiError::bad_request(format!(
            "field `{field_name}` must be {expected}",
        )));
    };

    if require_integer && value.as_i64().is_none() && value.as_u64().is_none() {
        return Err(ApiError::bad_request(format!(
            "field `{field_name}` must be an integer",
        )));
    }

    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if number < minimum {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must be at least {minimum}",
            )));
        }
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
        if number > maximum {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must be at most {maximum}",
            )));
        }
    }

    Ok(value.clone())
}

const CODEX_APP_REQUEST_RESULT_MAX_BYTES: usize = 64 * 1024;
const CODEX_APP_REQUEST_RESULT_MAX_DEPTH: usize = 32;

fn validate_codex_app_request_result(result: Value) -> std::result::Result<Value, ApiError> {
    validate_codex_app_request_result_depth(&result, 0)?;
    let encoded = serde_json::to_vec(&result).map_err(|err| {
        ApiError::bad_request(format!(
            "Codex app request result could not be serialized as JSON: {err}"
        ))
    })?;
    if encoded.len() > CODEX_APP_REQUEST_RESULT_MAX_BYTES {
        return Err(ApiError::bad_request(format!(
            "Codex app request result must be at most {} KB",
            CODEX_APP_REQUEST_RESULT_MAX_BYTES / 1024
        )));
    }
    Ok(result)
}

fn validate_codex_app_request_result_depth(
    value: &Value,
    depth: usize,
) -> std::result::Result<(), ApiError> {
    if depth > CODEX_APP_REQUEST_RESULT_MAX_DEPTH {
        return Err(ApiError::bad_request(format!(
            "Codex app request result must be at most {CODEX_APP_REQUEST_RESULT_MAX_DEPTH} levels deep",
        )));
    }

    match value {
        Value::Array(values) => {
            for entry in values {
                validate_codex_app_request_result_depth(entry, depth + 1)?;
            }
        }
        Value::Object(entries) => {
            for entry in entries.values() {
                validate_codex_app_request_result_depth(entry, depth + 1)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn validate_codex_mcp_elicitation_string_value(
    field_name: &str,
    schema: &Value,
    value: &Value,
) -> std::result::Result<Value, ApiError> {
    let Some(text) = value.as_str() else {
        return Err(ApiError::bad_request(format!(
            "field `{field_name}` must be a string",
        )));
    };

    if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64) {
        if text.chars().count() < min_length as usize {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must be at least {min_length} characters",
            )));
        }
    }
    if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64) {
        if text.chars().count() > max_length as usize {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must be at most {max_length} characters",
            )));
        }
    }

    if let Some(options) = codex_mcp_elicitation_string_options(schema) {
        if !options.iter().any(|option| option == text) {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must use one of the provided options",
            )));
        }
    }

    Ok(Value::String(text.to_owned()))
}

fn validate_codex_mcp_elicitation_array_value(
    field_name: &str,
    schema: &Value,
    value: &Value,
) -> std::result::Result<Value, ApiError> {
    let values = value.as_array().ok_or_else(|| {
        ApiError::bad_request(format!("field `{field_name}` must be a list"))
    })?;

    if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64) {
        if values.len() < min_items as usize {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must include at least {min_items} selections",
            )));
        }
    }
    if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64) {
        if values.len() > max_items as usize {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` must include at most {max_items} selections",
            )));
        }
    }

    let item_schema = schema.get("items").ok_or_else(|| {
        ApiError::bad_request(format!(
            "field `{field_name}` is missing its MCP elicitation item schema",
        ))
    })?;
    let allowed = codex_mcp_elicitation_array_options(item_schema);
    let mut normalized = Vec::with_capacity(values.len());
    for entry in values {
        let Some(text) = entry.as_str() else {
            return Err(ApiError::bad_request(format!(
                "field `{field_name}` only accepts string selections",
            )));
        };
        if let Some(options) = allowed.as_ref() {
            if !options.iter().any(|option| option == text) {
                return Err(ApiError::bad_request(format!(
                    "field `{field_name}` must use one of the provided options",
                )));
            }
        }
        normalized.push(Value::String(text.to_owned()));
    }

    Ok(Value::Array(normalized))
}

fn codex_mcp_elicitation_string_options(schema: &Value) -> Option<Vec<String>> {
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        let collected = options
            .iter()
            .filter_map(|option| option.get("const").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !collected.is_empty() {
            return Some(collected);
        }
    }

    let collected = schema
        .get("enum")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (!collected.is_empty()).then_some(collected)
}

fn codex_mcp_elicitation_array_options(schema: &Value) -> Option<Vec<String>> {
    if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
        let collected = options
            .iter()
            .filter_map(|option| option.get("const").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !collected.is_empty() {
            return Some(collected);
        }
    }

    let collected = schema
        .get("enum")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (!collected.is_empty()).then_some(collected)
}

fn queue_prompt_on_record(
    record: &mut SessionRecord,
    pending_prompt: PendingPrompt,
    attachments: Vec<PromptImageAttachment>,
) {
    record.queued_prompts.push_back(QueuedPromptRecord {
        attachments,
        pending_prompt,
    });
    sync_pending_prompts(record);
}

fn recover_interrupted_session_record(record: &mut SessionRecord) -> Option<String> {
    if !matches!(
        record.session.status,
        SessionStatus::Active | SessionStatus::Approval
    ) {
        return None;
    }

    let interrupted_interaction_count =
        expire_pending_interaction_messages(&mut record.session.messages);
    fail_running_command_messages(&mut record.session.messages);

    let mut notice = if interrupted_interaction_count > 0
        || record.session.status == SessionStatus::Approval
    {
        "TermAl restarted while this session was waiting for approval or input. That request expired. Send another prompt to continue.".to_owned()
    } else {
        "TermAl restarted before this turn finished. The last response may be incomplete. Send another prompt to continue.".to_owned()
    };

    let queued_count = record.queued_prompts.len();
    if queued_count > 0 {
        let noun = if queued_count == 1 {
            "prompt remains"
        } else {
            "prompts remain"
        };
        notice.push_str(&format!(" {queued_count} queued {noun} saved."));
    }

    Some(notice)
}

fn expire_pending_interaction_messages(messages: &mut [Message]) -> usize {
    let mut count = 0;
    for message in messages {
        match message {
            Message::Approval { decision, .. } => {
                if *decision == ApprovalDecision::Pending {
                    *decision = ApprovalDecision::Interrupted;
                    count += 1;
                }
            }
            Message::UserInputRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Interrupted;
                    count += 1;
                }
            }
            Message::McpElicitationRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Interrupted;
                    count += 1;
                }
            }
            Message::CodexAppRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Interrupted;
                    count += 1;
                }
            }
            _ => {}
        }
    }
    count
}

fn cancel_pending_interaction_messages(messages: &mut [Message]) {
    for message in messages {
        match message {
            Message::Approval { decision, .. } => {
                if *decision == ApprovalDecision::Pending {
                    *decision = ApprovalDecision::Rejected;
                }
            }
            Message::UserInputRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Canceled;
                }
            }
            Message::McpElicitationRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Canceled;
                }
            }
            Message::CodexAppRequest { state, .. } => {
                if *state == InteractionRequestState::Pending {
                    *state = InteractionRequestState::Canceled;
                }
            }
            _ => {}
        }
    }
}

fn fail_running_command_messages(messages: &mut [Message]) {
    for message in messages {
        if let Message::Command { status, .. } = message {
            if *status == CommandStatus::Running {
                *status = CommandStatus::Error;
            }
        }
    }
}

fn build_message_positions(messages: &[Message]) -> HashMap<String, usize> {
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| (message.id().to_owned(), index))
        .collect()
}

fn message_index_on_record(record: &mut SessionRecord, message_id: &str) -> Option<usize> {
    if let Some(index) = record.message_positions.get(message_id).copied() {
        if record
            .session
            .messages
            .get(index)
            .is_some_and(|message| message.id() == message_id)
        {
            return Some(index);
        }
    }

    record.message_positions = build_message_positions(&record.session.messages);
    record.message_positions.get(message_id).copied()
}

fn insert_message_on_record(record: &mut SessionRecord, index: usize, message: Message) -> usize {
    let index = index.min(record.session.messages.len());
    record.session.messages.insert(index, message);
    record.message_positions = build_message_positions(&record.session.messages);
    index
}

fn push_message_on_record(record: &mut SessionRecord, message: Message) -> usize {
    insert_message_on_record(record, record.session.messages.len(), message)
}

fn push_session_markdown_note_on_record(
    record: &mut SessionRecord,
    message_id: String,
    title: &str,
    markdown: String,
) {
    let message = Message::Markdown {
        id: message_id,
        timestamp: stamp_now(),
        author: Author::Assistant,
        title: title.to_owned(),
        markdown,
    };
    if let Some(preview) = message.preview_text() {
        record.session.preview = preview;
    }
    push_message_on_record(record, message);
}

fn replace_session_messages_on_record(
    record: &mut SessionRecord,
    messages: Vec<Message>,
    fallback_preview: Option<String>,
) {
    record.session.messages = messages;
    record.message_positions = build_message_positions(&record.session.messages);
    record.session.preview = record
        .session
        .messages
        .iter()
        .rev()
        .find_map(Message::preview_text)
        .or(fallback_preview)
        .unwrap_or_else(|| "Ready for a prompt.".to_owned());
}

fn codex_thread_messages_from_json(inner: &mut StateInner, thread: &Value) -> Option<Vec<Message>> {
    let turns = thread.get("turns").and_then(Value::as_array)?;
    let mut messages = Vec::new();
    for turn in turns {
        append_codex_thread_turn_messages(inner, turn, &mut messages)?;
    }
    (!messages.is_empty()).then_some(messages)
}

fn append_codex_thread_turn_messages(
    inner: &mut StateInner,
    turn: &Value,
    messages: &mut Vec<Message>,
) -> Option<()> {
    let items = turn.get("items").and_then(Value::as_array)?;
    for item in items {
        append_codex_thread_item_messages(inner, item, messages);
    }
    if let Some(text) = codex_thread_turn_status_text(turn) {
        messages.push(Message::Text {
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text,
            expanded_text: None,
        });
    }
    Some(())
}

fn append_codex_thread_item_messages(
    inner: &mut StateInner,
    item: &Value,
    messages: &mut Vec<Message>,
) {
    match item.get("type").and_then(Value::as_str) {
        Some("userMessage") => {
            if let Some(text) = codex_thread_user_message_text(item) {
                messages.push(Message::Text {
                    attachments: Vec::new(),
                    id: inner.next_message_id(),
                    timestamp: stamp_now(),
                    author: Author::You,
                    text,
                    expanded_text: None,
                });
            }
        }
        Some("agentMessage") => {
            let Some(text) = item
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return;
            };
            messages.push(Message::Text {
                attachments: Vec::new(),
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: text.to_owned(),
                expanded_text: None,
            });
        }
        Some("reasoning") => {
            let lines = codex_thread_reasoning_lines(item);
            if lines.is_empty() {
                return;
            }
            messages.push(Message::Thinking {
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Codex reasoning".to_owned(),
                lines,
            });
        }
        Some("plan") => {
            let Some(text) = item
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return;
            };
            messages.push(Message::Markdown {
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Codex plan".to_owned(),
                markdown: text.to_owned(),
            });
        }
        Some("commandExecution") => {
            let Some(command) = item
                .get("command")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return;
            };
            messages.push(Message::Command {
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                command: command.to_owned(),
                command_language: Some(shell_language().to_owned()),
                output: item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                output_language: infer_command_output_language(command).map(str::to_owned),
                status: codex_thread_command_status(item),
            });
        }
        Some("fileChange") => {
            let Some(changes) = item.get("changes").and_then(Value::as_array) else {
                return;
            };
            if item.get("status").and_then(Value::as_str) != Some("completed") {
                return;
            }
            for change in changes {
                let Some(file_path) = change
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                let diff = change.get("diff").and_then(Value::as_str).unwrap_or("");
                if diff.trim().is_empty() {
                    continue;
                }
                let change_type = match change.pointer("/kind/type").and_then(Value::as_str) {
                    Some("add") => ChangeType::Create,
                    _ => ChangeType::Edit,
                };
                let summary = match change_type {
                    ChangeType::Create => format!("Created {}", short_file_name(file_path)),
                    ChangeType::Edit => format!("Updated {}", short_file_name(file_path)),
                };
                let message_id = inner.next_message_id();
                messages.push(Message::Diff {
                    id: message_id.clone(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    change_set_id: Some(diff_change_set_id(&message_id)),
                    file_path: file_path.to_owned(),
                    summary,
                    diff: diff.to_owned(),
                    language: Some("diff".to_owned()),
                    change_type,
                });
            }
        }
        Some(item_type) => {
            let Some(markdown) = codex_thread_fallback_markdown(item, item_type) else {
                return;
            };
            messages.push(Message::Markdown {
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: codex_thread_fallback_title(item_type),
                markdown,
            });
        }
        None => {}
    }
}

fn codex_thread_user_message_text(item: &Value) -> Option<String> {
    let content = item.get("content").and_then(Value::as_array)?;
    let parts: Vec<String> = content
        .iter()
        .filter_map(codex_thread_user_input_text)
        .collect();
    let joined = parts.join("\n\n");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn codex_thread_user_input_text(input: &Value) -> Option<String> {
    match input.get("type").and_then(Value::as_str) {
        Some("text") => input
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        Some("image") => input
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!("Image: {value}")),
        Some("localImage") => input
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!("Local image: {value}")),
        Some("skill") => codex_thread_named_path_text(input, "Skill"),
        Some("mention") => codex_thread_named_path_text(input, "Mention"),
        _ => None,
    }
}

fn codex_thread_named_path_text(input: &Value, label: &str) -> Option<String> {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (name, path) {
        (Some(name), Some(path)) => Some(format!("{label}: {name} ({path})")),
        (Some(name), None) => Some(format!("{label}: {name}")),
        (None, Some(path)) => Some(format!("{label}: {path}")),
        (None, None) => None,
    }
}

fn codex_thread_reasoning_lines(item: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for key in ["summary", "content"] {
        let Some(values) = item.get(key).and_then(Value::as_array) else {
            continue;
        };
        for value in values {
            let Some(text) = value.as_str() else {
                continue;
            };
            for line in text.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    lines.push(trimmed.to_owned());
                }
            }
        }
    }
    lines
}

fn codex_thread_command_status(item: &Value) -> CommandStatus {
    match item.get("status").and_then(Value::as_str) {
        Some("completed") => match item.get("exitCode").and_then(Value::as_i64) {
            Some(0) | None => CommandStatus::Success,
            Some(_) => CommandStatus::Error,
        },
        Some("failed") | Some("declined") => CommandStatus::Error,
        _ => CommandStatus::Running,
    }
}

fn codex_thread_turn_status_text(turn: &Value) -> Option<String> {
    match turn.get("status").and_then(Value::as_str) {
        Some("failed") => {
            let detail = turn
                .get("error")
                .filter(|value| !value.is_null())
                .map(summarize_error)
                .unwrap_or_else(|| "Codex reported a turn failure.".to_owned());
            Some(format!("Turn failed: {detail}"))
        }
        Some("interrupted") => Some("Turn interrupted.".to_owned()),
        _ => None,
    }
}

fn codex_thread_fallback_title(item_type: &str) -> String {
    match item_type {
        "mcpToolCall" => "Codex MCP tool call".to_owned(),
        "dynamicToolCall" => "Codex dynamic tool call".to_owned(),
        _ => format!("Codex {item_type}"),
    }
}

fn codex_thread_fallback_markdown(item: &Value, item_type: &str) -> Option<String> {
    let mut sections = vec![format!("Codex returned a `{item_type}` thread item.")];
    if let Some(tool) = item
        .get("tool")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(format!("Tool: `{tool}`"));
    }
    if let Some(server) = item
        .get("server")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(format!("Server: `{server}`"));
    }
    if let Some(status) = item
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(format!("Status: `{status}`"));
    }
    if let Some(prompt) = item
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        sections.push(prompt.to_owned());
    }
    if let Some(error) = item.get("error").filter(|value| !value.is_null()) {
        sections.push(format!("Error: {}", summarize_error(error)));
    }
    Some(sections.join("\n\n"))
}

fn codex_approval_policy_from_json_value(value: &Value) -> Option<CodexApprovalPolicy> {
    match value {
        Value::String(raw) => match raw.as_str() {
            "untrusted" => Some(CodexApprovalPolicy::Untrusted),
            "on-failure" => Some(CodexApprovalPolicy::OnFailure),
            "on-request" => Some(CodexApprovalPolicy::OnRequest),
            "never" => Some(CodexApprovalPolicy::Never),
            _ => None,
        },
        _ => None,
    }
}

fn codex_reasoning_effort_from_json_value(value: &Value) -> Option<CodexReasoningEffort> {
    match value {
        Value::String(raw) => match raw.as_str() {
            "none" => Some(CodexReasoningEffort::None),
            "minimal" => Some(CodexReasoningEffort::Minimal),
            "low" => Some(CodexReasoningEffort::Low),
            "medium" => Some(CodexReasoningEffort::Medium),
            "high" => Some(CodexReasoningEffort::High),
            "xhigh" => Some(CodexReasoningEffort::XHigh),
            _ => None,
        },
        _ => None,
    }
}

fn codex_sandbox_mode_from_json_value(value: &Value) -> Option<CodexSandboxMode> {
    match value {
        Value::String(raw) => match raw.as_str() {
            "danger-full-access" => Some(CodexSandboxMode::DangerFullAccess),
            "read-only" => Some(CodexSandboxMode::ReadOnly),
            "workspace-write" => Some(CodexSandboxMode::WorkspaceWrite),
            _ => None,
        },
        Value::Object(_) => match value.get("type").and_then(Value::as_str) {
            Some("dangerFullAccess") => Some(CodexSandboxMode::DangerFullAccess),
            Some("readOnly") => Some(CodexSandboxMode::ReadOnly),
            Some("workspaceWrite") => Some(CodexSandboxMode::WorkspaceWrite),
            _ => None,
        },
        _ => None,
    }
}

fn default_forked_codex_session_name(current_name: &str, thread_name: Option<&str>) -> String {
    let trimmed_thread_name = thread_name
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let trimmed_current_name = current_name.trim();
    let base = trimmed_thread_name.unwrap_or(trimmed_current_name);
    format!("{base} Fork")
}

fn resolve_forked_codex_workdir(
    requested_workdir: Option<&str>,
    fallback_workdir: &str,
    project_id: Option<&str>,
    state: &AppState,
) -> Result<String, ApiError> {
    let Some(requested_workdir) = requested_workdir
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(fallback_workdir.to_owned());
    };

    let project_id = match project_id {
        Some(project_id) => project_id,
        None => return Ok(requested_workdir.to_owned()),
    };
    let project_root = resolve_project_root_path_by_id(state, project_id)?;
    if path_contains(project_root.to_string_lossy().as_ref(), FsPath::new(requested_workdir)) {
        Ok(requested_workdir.to_owned())
    } else {
        Ok(fallback_workdir.to_owned())
    }
}

struct CodexThreadActionContext {
    approval_policy: CodexApprovalPolicy,
    model: String,
    model_options: Vec<SessionModelOption>,
    name: String,
    project_id: Option<String>,
    reasoning_effort: CodexReasoningEffort,
    sandbox_mode: CodexSandboxMode,
    thread_id: String,
    thread_state: Option<CodexThreadState>,
    workdir: String,
}

#[derive(Clone)]
enum SessionRuntime {
    None,
    Claude(ClaudeRuntimeHandle),
    Codex(CodexRuntimeHandle),
    Acp(AcpRuntimeHandle),
}

#[derive(Clone)]
struct ClaudeRuntimeHandle {
    runtime_id: String,
    input_tx: Sender<ClaudeRuntimeCommand>,
    process: Arc<SharedChild>,
}

impl ClaudeRuntimeHandle {
    fn kill(&self) -> Result<()> {
        kill_child_process(&self.process, "Claude")
    }
}

#[derive(Clone)]
struct CodexRuntimeHandle {
    runtime_id: String,
    input_tx: Sender<CodexRuntimeCommand>,
    process: Arc<SharedChild>,
    shared_session: Option<SharedCodexSessionHandle>,
}

impl CodexRuntimeHandle {
    fn kill(&self) -> Result<()> {
        kill_child_process(&self.process, "Codex")
    }
}

#[derive(Clone)]
struct SharedCodexRuntime {
    runtime_id: String,
    input_tx: Sender<CodexRuntimeCommand>,
    process: Arc<SharedChild>,
    sessions: SharedCodexSessionMap,
    thread_sessions: SharedCodexThreadMap,
}

#[derive(Clone)]
struct SharedCodexSessionHandle {
    runtime: SharedCodexRuntime,
    session_id: String,
}

impl SharedCodexSessionHandle {
    fn detach(&self) {
        let removed_thread_id = {
            let mut sessions = self
                .runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            sessions
                .remove(&self.session_id)
                .and_then(|state| state.thread_id)
        };

        if let Some(thread_id) = removed_thread_id {
            self.runtime
                .thread_sessions
                .lock()
                .expect("shared Codex thread mutex poisoned")
                .remove(&thread_id);
        }
    }

    fn interrupt_turn(&self) -> Result<()> {
        let (thread_id, turn_id) = {
            let sessions = self
                .runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let Some(state) = sessions.get(&self.session_id) else {
                return Ok(());
            };
            let Some(thread_id) = state.thread_id.clone() else {
                return Ok(());
            };
            let Some(turn_id) = state.turn_id.clone() else {
                return Ok(());
            };
            (thread_id, turn_id)
        };

        let (response_tx, response_rx) = mpsc::channel();
        self.runtime
            .input_tx
            .send(CodexRuntimeCommand::InterruptTurn {
                response_tx,
                thread_id,
                turn_id,
            })
            .map_err(|err| anyhow!("failed to queue Codex turn interrupt: {err}"))?;

        match response_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(detail)) => Err(anyhow!(detail)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(anyhow!("timed out waiting for Codex turn interrupt"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(anyhow!("Codex turn interrupt did not return a result"))
            }
        }
    }

    fn interrupt_and_detach(&self) -> Result<()> {
        let result = self.interrupt_turn();
        self.detach();
        result
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcpAgent {
    Cursor,
    Gemini,
}

impl AcpAgent {
    fn agent(self) -> Agent {
        match self {
            Self::Cursor => Agent::Cursor,
            Self::Gemini => Agent::Gemini,
        }
    }

    fn command(self, launch_options: AcpLaunchOptions) -> Result<Command> {
        match self {
            Self::Cursor => {
                let exe = find_command_on_path("cursor-agent")
                    .ok_or_else(|| anyhow!("`cursor-agent` was not found on PATH"))?;
                let mut command = Command::new(exe);
                command.arg("acp");
                Ok(command)
            }
            Self::Gemini => {
                let exe = find_command_on_path("gemini")
                    .ok_or_else(|| anyhow!("`gemini` was not found on PATH"))?;
                let mut command = Command::new(exe);
                command.arg("--acp");
                if let Some(approval_mode) = launch_options.gemini_approval_mode {
                    command.args(["--approval-mode", approval_mode.as_cli_value()]);
                }
                Ok(command)
            }
        }
    }

    fn label(self) -> &'static str {
        self.agent().name()
    }
}

#[derive(Clone)]
struct AcpRuntimeHandle {
    agent: AcpAgent,
    runtime_id: String,
    input_tx: Sender<AcpRuntimeCommand>,
    process: Arc<SharedChild>,
}

impl AcpRuntimeHandle {
    fn kill(&self) -> Result<()> {
        kill_child_process(&self.process, self.agent.label())
    }
}

#[derive(Clone)]
enum KillableRuntime {
    Claude(ClaudeRuntimeHandle),
    Codex(CodexRuntimeHandle),
    Acp(AcpRuntimeHandle),
}

impl KillableRuntime {
    fn kill(&self) -> Result<()> {
        match self {
            Self::Claude(handle) => handle.kill(),
            Self::Codex(handle) => handle.kill(),
            Self::Acp(handle) => handle.kill(),
        }
    }
}

fn shutdown_removed_runtime(runtime: KillableRuntime, context: &str) -> Result<()> {
    match runtime {
        KillableRuntime::Codex(handle) => {
            if let Some(shared_session) = &handle.shared_session {
                match shared_session.interrupt_and_detach() {
                    Ok(()) => {
                        Ok(())
                    }
                    Err(interrupt_err) => {
                        if shared_child_has_exited(&handle.process, "shared Codex runtime")? {
                            Err(anyhow!(
                                "shared Codex runtime had already exited while removing {context}: {interrupt_err:#}"
                            ))
                        } else {
                            Err(anyhow!(
                                "failed to interrupt shared Codex turn for {context}: {interrupt_err:#}"
                            ))
                        }
                    }
                }
            } else {
                handle
                    .kill()
                    .with_context(|| format!("failed to kill Codex runtime for {context}"))
            }
        }
        KillableRuntime::Claude(handle) => handle
            .kill()
            .with_context(|| format!("failed to kill Claude runtime for {context}")),
        KillableRuntime::Acp(handle) => handle
            .kill()
            .with_context(|| format!("failed to kill {} runtime for {context}", handle.agent.label())),
    }
}

#[derive(Clone)]
enum RuntimeToken {
    Claude(String),
    Codex(String),
    Acp(String),
}

impl SessionRuntime {
    fn matches_runtime_token(&self, token: &RuntimeToken) -> bool {
        match (self, token) {
            (Self::Claude(handle), RuntimeToken::Claude(runtime_id)) => {
                handle.runtime_id == *runtime_id
            }
            (Self::Codex(handle), RuntimeToken::Codex(runtime_id)) => {
                handle.runtime_id == *runtime_id
            }
            (Self::Acp(handle), RuntimeToken::Acp(runtime_id)) => handle.runtime_id == *runtime_id,
            _ => false,
        }
    }
}

fn kill_child_process(process: &Arc<SharedChild>, label: &str) -> Result<()> {
    if wait_for_shared_child_exit_timeout(
        process,
        Duration::from_millis(50),
        label,
    )?
    .is_some()
    {
        return Ok(());
    }

    match process.kill() {
        Ok(()) => Ok(()),
        Err(err) => {
            if wait_for_shared_child_exit_timeout(
                process,
                Duration::from_millis(50),
                label,
            )?
            .is_some()
            {
                Ok(())
            } else {
                Err(err).with_context(|| format!("failed to terminate {label} process"))
            }
        }
    }
}

fn shared_child_has_exited(process: &Arc<SharedChild>, label: &str) -> Result<bool> {
    match process.try_wait() {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(err) => Err(anyhow!("failed checking {label} process status: {err}")),
    }
}

fn wait_for_shared_child_exit_timeout(
    process: &Arc<SharedChild>,
    timeout: Duration,
    label: &str,
) -> Result<Option<std::process::ExitStatus>> {
    match process.try_wait() {
        Ok(Some(status)) => return Ok(Some(status)),
        Ok(None) => {}
        Err(err) => return Err(anyhow!("failed waiting for {label} process: {err}")),
    }

    let wait_process = process.clone();
    let (status_tx, status_rx) = mpsc::sync_channel(1);
    // If the timeout elapses, callers either terminate the process immediately or continue with
    // a long-lived shared child. The waiter is detached so we never block the caller thread.
    std::thread::spawn(move || {
        let _ = status_tx.send(wait_process.wait());
    });

    match status_rx.recv_timeout(timeout) {
        Ok(Ok(status)) => Ok(Some(status)),
        Ok(Err(err)) => Err(anyhow!("failed waiting for {label} process: {err}")),
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!(
            "failed waiting for {label} process: wait thread disconnected"
        )),
    }
}
