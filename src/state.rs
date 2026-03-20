#[derive(Clone)]
struct AppState {
    default_workdir: String,
    persistence_path: Arc<PathBuf>,
    state_events: broadcast::Sender<String>,
    delta_events: broadcast::Sender<String>,
    shared_codex_runtime: Arc<Mutex<Option<SharedCodexRuntime>>>,
    remote_registry: Arc<RemoteRegistry>,
    inner: Arc<Mutex<StateInner>>,
}

impl AppState {
    fn new(default_workdir: String) -> Result<Self> {
        let persistence_path = resolve_persistence_path(&default_workdir);
        let inner = load_state(&persistence_path)?.unwrap_or_else(|| {
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

        let state = Self {
            default_workdir,
            persistence_path: Arc::new(persistence_path),
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

        let session = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner.sessions[index].session.clone()
        };

        Ok(AgentCommandsResponse {
            commands: read_claude_agent_commands(FsPath::new(&session.workdir))?,
        })
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
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner.sessions[index].session.clone()
        };

        search_instruction_phrase(FsPath::new(&session.workdir), query)
    }

    fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<CreateSessionResponse, ApiError> {
        let requested_workdir = request
            .workdir
            .as_deref()
            .map(resolve_session_workdir)
            .transpose()?;
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
        let agent = request.agent.unwrap_or(Agent::Codex);
        validate_agent_session_setup(agent, &workdir).map_err(ApiError::bad_request)?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let mut record = inner.create_session(
            agent,
            request.name,
            workdir,
            project.as_ref().map(|entry| entry.id.clone()),
            request
                .model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
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
        match record.session.agent {
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
        if let Some(slot) = inner
            .find_session_index(&record.session.id)
            .and_then(|index| inner.sessions.get_mut(index))
        {
            *slot = record.clone();
        }
        self.commit_locked(&mut inner)
            .map_err(|err| ApiError::internal(format!("failed to persist session: {err:#}")))?;
        Ok(CreateSessionResponse {
            session_id: record.session.id,
            state: self.snapshot_from_inner(&inner),
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
                .map(|record| record.session.clone())
                .collect(),
        }
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
            .find_session_index(session_id)
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
            .find_session_index(session_id)
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
            .find_session_index(session_id)
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

    fn allocate_message_id(&self) -> String {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner.next_message_id()
    }

    fn set_external_session_id(&self, session_id: &str, external_session_id: String) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index].external_session_id = Some(external_session_id.clone());
        inner.sessions[index].session.external_session_id = Some(external_session_id);
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

    fn set_codex_runtime(&self, session_id: &str, handle: CodexRuntimeHandle) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        Ok(())
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
        inner.sessions[index].runtime = SessionRuntime::None;
        inner.sessions[index].runtime_reset_required = false;
        inner.sessions[index].pending_claude_approvals.clear();
        inner.sessions[index].pending_codex_approvals.clear();
        inner.sessions[index].pending_acp_approvals.clear();
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
            record.pending_claude_approvals.clear();
            record.pending_codex_approvals.clear();
            record.pending_acp_approvals.clear();

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

        sync_session_approval_state(record, ApprovalDecision::Canceled);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    fn kill_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_kill_session(session_id);
        }
        let runtime_to_kill = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &mut inner.sessions[index];

            let runtime = match &record.runtime {
                SessionRuntime::Claude(handle) => Some(KillableRuntime::Claude(handle.clone())),
                SessionRuntime::Codex(handle) => Some(KillableRuntime::Codex(handle.clone())),
                SessionRuntime::Acp(handle) => Some(KillableRuntime::Acp(handle.clone())),
                SessionRuntime::None => None,
            };

            if runtime.is_some()
                && matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval
                )
            {
                record.session.status = SessionStatus::Idle;
                record.session.preview = "Stopping session\u{2026}".to_owned();
                self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!("failed to persist session state: {err:#}"))
                })?;
            }

            runtime
        };

        if let Some(runtime) = runtime_to_kill {
            match runtime {
                KillableRuntime::Codex(handle) => {
                    if let Some(shared_session) = &handle.shared_session {
                        shared_session.interrupt_turn().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to interrupt Codex session: {err:#}"
                            ))
                        })?;
                        shared_session.detach();
                    } else {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!("failed to kill session: {err:#}"))
                        })?;
                    }
                }
                runtime => runtime.kill().map_err(|err| {
                    ApiError::internal(format!("failed to kill session: {err:#}"))
                })?,
            }
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        inner.sessions.remove(index);

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
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
            .find_session_index(session_id)
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
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &mut inner.sessions[index];

            if !matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval
            ) {
                return Err(ApiError::conflict("session is not currently running"));
            }

            for message in &mut record.session.messages {
                if let Message::Approval { decision, .. } = message {
                    if *decision == ApprovalDecision::Pending {
                        *decision = ApprovalDecision::Rejected;
                    }
                }
            }

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

        match runtime_to_stop {
            KillableRuntime::Codex(handle) => {
                if let Some(shared_session) = &handle.shared_session {
                    shared_session.interrupt_turn().map_err(|err| {
                        ApiError::internal(format!("failed to stop session: {err:#}"))
                    })?;
                    shared_session.detach();
                } else {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!("failed to stop session: {err:#}"))
                    })?;
                }
            }
            runtime => runtime
                .kill()
                .map_err(|err| ApiError::internal(format!("failed to stop session: {err:#}")))?,
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
                if matches!(message, Message::Approval { .. }) {
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
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = &mut inner.sessions[index];

        if let Some(message_index) = message_index_on_record(record, message_id) {
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
                }
                _ => {
                    return Err(anyhow!(
                        "session `{session_id}` message `{message_id}` is not a parallel-agents message"
                    ));
                }
            }
        } else {
            push_message_on_record(
                record,
                Message::ParallelAgents {
                    id: message_id.to_owned(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    agents: agents.clone(),
                },
            );
        }

        record.session.preview = parallel_agents_preview_text(&agents);
        self.commit_locked(&mut inner)?;
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
            .find_session_index(session_id)
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
                .send(CodexRuntimeCommand::ApprovalResponse {
                    response: CodexApprovalResponseCommand {
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
        sync_session_approval_state(record, decision);
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
    let decision_value = match kind {
        CodexApprovalKind::CommandExecution => match decision {
            ApprovalDecision::Accepted => json!("accept"),
            ApprovalDecision::AcceptedForSession => json!("acceptForSession"),
            ApprovalDecision::Rejected => json!("decline"),
            ApprovalDecision::Pending
            | ApprovalDecision::Interrupted
            | ApprovalDecision::Canceled => {
                unreachable!("non-deliverable approval decisions are not sent to Codex")
            }
        },
        CodexApprovalKind::FileChange => match decision {
            ApprovalDecision::Accepted => json!("accept"),
            ApprovalDecision::AcceptedForSession => json!("acceptForSession"),
            ApprovalDecision::Rejected => json!("decline"),
            ApprovalDecision::Pending
            | ApprovalDecision::Interrupted
            | ApprovalDecision::Canceled => {
                unreachable!("non-deliverable approval decisions are not sent to Codex")
            }
        },
    };

    json!({ "decision": decision_value })
}

fn normalize_remote_configs(remotes: Vec<RemoteConfig>) -> Result<Vec<RemoteConfig>, ApiError> {
    let mut normalized = vec![RemoteConfig::local()];
    let mut seen_ids = HashSet::from([default_local_remote_id()]);

    for remote in remotes {
        let id = remote.id.trim();
        if id.is_empty() {
            return Err(ApiError::bad_request("remote id cannot be empty"));
        }
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
                let host = remote.host.as_deref().map(str::trim).unwrap_or("");
                if host.is_empty() {
                    return Err(ApiError::bad_request(format!(
                        "ssh remote `{id}` must have a host"
                    )));
                }
                normalized.push(RemoteConfig {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    transport: RemoteTransport::Ssh,
                    enabled: remote.enabled,
                    host: Some(host.to_owned()),
                    port: Some(remote.port.unwrap_or(DEFAULT_SSH_REMOTE_PORT)),
                    user: remote
                        .user
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned),
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
            codex_approval_policy: default_codex_approval_policy(),
            codex_reasoning_effort: self.preferences.default_codex_reasoning_effort,
            codex_sandbox_mode: default_codex_sandbox_mode(),
            external_session_id: None,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            queued_prompts: VecDeque::new(),
            message_positions: HashMap::new(),
            remote_id: None,
            remote_session_id: None,
            runtime: SessionRuntime::None,
            runtime_reset_required: false,
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
                project.remote_id == LOCAL_REMOTE_ID && path_contains(&project.root_path, target)
            })
            .max_by_key(|project| project.root_path.len())
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
            sessions: inner
                .sessions
                .iter()
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
            codex_approval_policy: self.codex_approval_policy,
            codex_reasoning_effort: self.codex_reasoning_effort,
            codex_sandbox_mode: self.codex_sandbox_mode,
            external_session_id: self.external_session_id,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            queued_prompts: self.queued_prompts,
            message_positions: build_message_positions(&session.messages),
            remote_id: self.remote_id,
            remote_session_id: self.remote_session_id,
            runtime: SessionRuntime::None,
            runtime_reset_required: false,
            session,
        };
        sync_pending_prompts(&mut record);
        record
    }
}

#[derive(Clone)]
struct SessionRecord {
    active_codex_approval_policy: Option<CodexApprovalPolicy>,
    active_codex_reasoning_effort: Option<CodexReasoningEffort>,
    active_codex_sandbox_mode: Option<CodexSandboxMode>,
    codex_approval_policy: CodexApprovalPolicy,
    codex_reasoning_effort: CodexReasoningEffort,
    codex_sandbox_mode: CodexSandboxMode,
    external_session_id: Option<String>,
    pending_claude_approvals: HashMap<String, ClaudePendingApproval>,
    pending_codex_approvals: HashMap<String, CodexPendingApproval>,
    pending_acp_approvals: HashMap<String, AcpPendingApproval>,
    queued_prompts: VecDeque<QueuedPromptRecord>,
    message_positions: HashMap<String, usize>,
    remote_id: Option<String>,
    remote_session_id: Option<String>,
    runtime: SessionRuntime,
    runtime_reset_required: bool,
    session: Session,
}

impl SessionRecord {
    fn is_remote_proxy(&self) -> bool {
        self.remote_id.is_some() && self.remote_session_id.is_some()
    }
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

fn session_has_live_approvals(record: &SessionRecord) -> bool {
    record.session.messages.iter().any(|message| {
        matches!(
            message,
            Message::Approval {
                decision: ApprovalDecision::Pending,
                ..
            }
        )
    })
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

fn sync_session_approval_state(record: &mut SessionRecord, resolved_decision: ApprovalDecision) {
    if session_has_live_approvals(record) {
        record.session.status = SessionStatus::Approval;
        record.session.preview =
            approval_preview_text(record.session.agent.name(), ApprovalDecision::Pending);
        return;
    }

    if matches!(
        record.session.status,
        SessionStatus::Approval | SessionStatus::Active
    ) {
        record.session.status = SessionStatus::Active;
        record.session.preview =
            approval_preview_text(record.session.agent.name(), resolved_decision);
    }
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

    let interrupted_approval_count = expire_pending_approval_messages(&mut record.session.messages);
    fail_running_command_messages(&mut record.session.messages);

    let mut notice = if interrupted_approval_count > 0
        || record.session.status == SessionStatus::Approval
    {
        "TermAl restarted while this session was waiting for approval. That request expired. Send another prompt to continue.".to_owned()
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

fn expire_pending_approval_messages(messages: &mut [Message]) -> usize {
    let mut count = 0;
    for message in messages {
        if let Message::Approval { decision, .. } = message {
            if *decision == ApprovalDecision::Pending {
                *decision = ApprovalDecision::Interrupted;
                count += 1;
            }
        }
    }
    count
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
    fn ensure_registered(&self) {
        let mut sessions = self
            .runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        sessions.entry(self.session_id.clone()).or_default();
    }

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
    match process.try_wait() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => process
            .kill()
            .with_context(|| format!("failed to terminate {label} process")),
        Err(err) => Err(anyhow!("failed to inspect {label} process state: {err}")),
    }
}
