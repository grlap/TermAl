// Host-local project environment settings and their process-local selection.
// Project persistence owns values; client projections expose none. Provider
// adapters apply one selection at start/resume. Declared checks retain only
// missing names, never values, so settings changes cannot retarget a run.

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
struct ProjectEnvironment(BTreeMap<String, String>);

impl std::fmt::Debug for ProjectEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectEnvironment").field("names", &self.0.keys()).finish()
    }
}

impl ProjectEnvironment {
    fn is_empty(&self) -> bool { self.0.is_empty() }

    fn validate(&self) -> Result<(), ApiError> {
        if self.0.len() > 64 || self.0.values().map(String::len).sum::<usize>() > 64 * 1024 {
            return Err(ApiError::bad_request("project environment exceeds its size cap"));
        }
        if self.0.iter().any(|(name, value)| !project_environment_name(name)
            || value.len() > 8192 || value.contains('\0')) {
            return Err(ApiError::bad_request("invalid project environment name or value"));
        }
        if cfg!(windows) && self.0.keys().map(|name| project_environment_key(name))
            .collect::<BTreeSet<_>>().len() != self.0.len() {
            return Err(ApiError::bad_request("duplicate project environment names"));
        }
        Ok(())
    }

    fn present_names(&self) -> BTreeSet<String> {
        self.0.iter().filter(|(_, value)| !value.trim().is_empty())
            .map(|(name, _)| project_environment_key(name)).collect()
    }

    fn runtime_selection(&self) -> Self {
        Self(self.0.iter().map(|(name, value)|
            (project_environment_key(name), value.clone())).collect())
    }
}

fn project_environment_key(name: &str) -> String {
    if cfg!(windows) { name.to_ascii_uppercase() } else { name.to_owned() }
}

fn project_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 128
        && bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        // These are owned by provider/host launch, never project settings.
        && !name.to_ascii_uppercase().starts_with("TERMAL_")
        && !name.to_ascii_uppercase().starts_with("ENGRAM_")
}

#[derive(Clone, Debug, Default)]
struct AppliedProjectEnvironment {
    values: ProjectEnvironment,
}

impl AppliedProjectEnvironment {
    fn apply(&self, command: &mut Command) {
        for (name, value) in &self.values.0 { command.env(name, value); }
    }

    fn bind(&self, binding: &mut EngramDeclaredBinding) {
        Self::bind_present_names(&self.values.present_names(), binding);
    }

    fn bind_present_names(present: &BTreeSet<String>, binding: &mut EngramDeclaredBinding) {
        binding.environment_missing = binding.entry.env.iter()
            .find(|name| !present.contains(&project_environment_key(name))).cloned();
    }
}

fn project_environment_for_session_locked(inner: &StateInner, session_id: &str) -> ProjectEnvironment {
    inner.find_session_index(session_id).and_then(|index| {
        let record = &inner.sessions[index];
        if !record.is_local_session() { return None; }
        inner.find_project(record.session.project_id.as_deref()?)
            .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
            .map(|project| project.environment.runtime_selection())
    }).unwrap_or_default()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateProjectEnvironmentRequest {
    environment: ProjectEnvironment,
}

async fn update_project_environment(
    AxumPath(project_id): AxumPath<String>, State(state): State<AppState>,
    request: Result<Json<UpdateProjectEnvironmentRequest>, JsonRejection>,
) -> Result<Json<StateResponse>, ApiError> {
    // Serde diagnostics can quote an input value. This endpoint never returns it.
    let Json(request) = request.map_err(|_| ApiError::bad_request("invalid project environment request"))?;
    let response = run_blocking_api(move || state.update_project_environment(&project_id, request.environment)).await?;
    Ok(Json(response))
}

impl AppState {
    fn update_project_environment(&self, project_id: &str, environment: ProjectEnvironment)
        -> Result<StateResponse, ApiError> {
        environment.validate()?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.projects.iter().position(|project| project.id == project_id)
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        if inner.projects[index].remote_id != LOCAL_REMOTE_ID {
            return Err(ApiError::bad_request("configure environment on the executing host's local project"));
        }
        let previous = std::mem::replace(&mut inner.projects[index].environment, environment);
        if self.commit_locked(&mut inner).is_err() {
            inner.projects[index].environment = previous;
            return Err(ApiError::internal("failed to persist project environment"));
        }
        Ok(self.snapshot_from_inner(&inner))
    }

    // Off-lock declaration read before a prompt reaches the provider. This
    // warns only; it neither blocks commands nor reads an agent shell's env.
    fn project_environment_notice(&self, session_id: &str) -> Option<String> {
        let (workdir, named, opening, applied) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let record = &inner.sessions[inner.find_session_index(session_id)?];
            if !record.is_local_session() { return None; }
            let project = inner.find_project(record.session.project_id.as_deref()?)?;
            if project.remote_id != LOCAL_REMOTE_ID { return None; }
            let opening = record.engram.declared_run_turn.as_ref()
                .filter(|turn| turn.is_current(record))
                .and_then(|turn| turn.snapshot.as_ref())
                .map(|snapshot| snapshot.entries.iter().map(|entry| entry.binding.entry.clone()).collect::<Vec<_>>());
            (record.session.workdir.clone(),
                record.engram.observed_source_root().map(|root| root.root.clone()),
                opening, record.project_environment.clone())
        };
        // Reuse the opening owner's exact declaration when available. Outside
        // native opening, resolve the executing worktree off the state lock;
        // the main project checkout remains only the settings owner.
        let entries = match opening {
            Some(entries) => entries,
            None => {
                let root = named.map(PathBuf::from)
                    .unwrap_or_else(|| engram_worktree_root_path(FsPath::new(&workdir)));
                let EngramDeclaration::Loaded { entries, .. } = engram_load_declaration(&root) else { return None; };
                entries
            }
        };
        let present = applied.values.present_names();
        let missing = entries.iter().flat_map(|entry| &entry.env)
            .filter(|name| !present.contains(&project_environment_key(name))).collect::<BTreeSet<_>>();
        if missing.is_empty() { return None; }
        let omitted = missing.len().saturating_sub(16);
        Some(format!("[TermAl] Declared test environment is missing on this executor: {}. Affected checks receive UNKNOWN and no native credit.",
            missing.into_iter().take(16).map(|name| format!("environment_missing:{name}"))
                .chain((omitted > 0).then(|| format!("{omitted} further names omitted")))
                .collect::<Vec<_>>().join(", ")))
    }
}

// Existing adapter fixtures exercise the no-project-settings path.
#[cfg(test)]
fn spawn_acp_runtime(state: AppState, session_id: String, cwd: String, agent: AcpAgent,
    gemini_approval_mode: Option<GeminiApprovalMode>, engram: Option<&TermalDelegationMcpStdioConfig>)
    -> Result<AcpRuntimeHandle> {
    spawn_acp_runtime_with_environment(state, session_id, cwd, agent, gemini_approval_mode,
        engram, &AppliedProjectEnvironment::default())
}

#[cfg(test)]
fn spawn_claude_runtime(state: AppState, session_id: String, cwd: String, model: String,
    approval_mode: ClaudeApprovalMode, effort: ClaudeEffortLevel, resume_session_id: Option<String>,
    delegation_mcp_config: String, engram: Option<&TermalDelegationMcpStdioConfig>,
    host_mcp_servers_only: bool,
    model_options_tx: Option<Sender<std::result::Result<Vec<SessionModelOption>, String>>>)
    -> Result<ClaudeRuntimeHandle> {
    spawn_claude_runtime_with_environment(state, session_id, cwd, model, approval_mode, effort,
        resume_session_id, delegation_mcp_config, engram, host_mcp_servers_only, model_options_tx,
        &AppliedProjectEnvironment::default())
}

#[cfg(test)]
impl AppState {
    fn start_turn_on_record(&self, record: &mut SessionRecord, message_id: String, prompt: String,
        attachments: Vec<PromptImageAttachment>, expanded_prompt: Option<String>,
        source: Option<MessageSource>, engram: Option<EngramMcpRuntimeConfig>, host_only: bool)
        -> std::result::Result<StartedTurn, ApiError> {
        self.start_turn_on_record_with_environment(record, message_id, prompt, attachments,
            expanded_prompt, source, engram, host_only, ProjectEnvironment::default())
    }
}
