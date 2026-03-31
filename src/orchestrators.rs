fn resolve_orchestrator_templates_path(default_workdir: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir).join("orchestrators.json")
}

fn load_orchestrator_template_store(path: &FsPath) -> Result<OrchestratorTemplateStore> {
    if !path.exists() {
        return Ok(OrchestratorTemplateStore::default());
    }

    let raw = fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    let mut store: OrchestratorTemplateStore = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse `{}`", path.display()))?;
    store.normalize();
    Ok(store)
}

fn persist_orchestrator_template_store(
    path: &FsPath,
    store: &OrchestratorTemplateStore,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let encoded = serde_json::to_vec_pretty(store)
        .context("failed to serialize orchestrator templates")?;
    fs::write(path, encoded).with_context(|| format!("failed to write `{}`", path.display()))
}

fn stamp_orchestrator_template_now() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn default_next_orchestrator_template_number() -> usize {
    1
}

const MAX_ORCHESTRATOR_TEMPLATE_SESSIONS: usize = 50;
const MAX_ORCHESTRATOR_TEMPLATE_TRANSITIONS: usize = 200;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorTemplateStore {
    #[serde(default = "default_next_orchestrator_template_number")]
    next_template_number: usize,
    #[serde(default)]
    templates: Vec<OrchestratorTemplate>,
}

impl Default for OrchestratorTemplateStore {
    fn default() -> Self {
        Self {
            next_template_number: default_next_orchestrator_template_number(),
            templates: Vec::new(),
        }
    }
}

impl OrchestratorTemplateStore {
    fn normalize(&mut self) {
        let max_number = self
            .templates
            .iter()
            .filter_map(|template| {
                template
                    .id
                    .strip_prefix("orchestrator-template-")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .max()
            .unwrap_or(0);

        self.next_template_number = self
            .next_template_number
            .max(max_number.saturating_add(1))
            .max(1);
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum OrchestratorTransitionTrigger {
    #[default]
    OnCompletion,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum OrchestratorTransitionResultMode {
    None,
    LastResponse,
    Summary,
    SummaryAndLastResponse,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorNodePosition {
    x: f64,
    y: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorSessionTemplate {
    id: String,
    name: String,
    agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default)]
    instructions: String,
    #[serde(default)]
    auto_approve: bool,
    position: OrchestratorNodePosition,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorTemplateTransition {
    id: String,
    from_session_id: String,
    to_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from_anchor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to_anchor: Option<String>,
    #[serde(default)]
    trigger: OrchestratorTransitionTrigger,
    result_mode: OrchestratorTransitionResultMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_template: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorTemplateDraft {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    #[serde(default)]
    sessions: Vec<OrchestratorSessionTemplate>,
    #[serde(default)]
    transitions: Vec<OrchestratorTemplateTransition>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorTemplate {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    #[serde(default)]
    sessions: Vec<OrchestratorSessionTemplate>,
    #[serde(default)]
    transitions: Vec<OrchestratorTemplateTransition>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorTemplatesResponse {
    templates: Vec<OrchestratorTemplate>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorTemplateResponse {
    template: OrchestratorTemplate,
}

impl AppState {
    fn list_orchestrator_templates(&self) -> Result<OrchestratorTemplatesResponse, ApiError> {
        let _guard = self.inner.lock().expect("state mutex poisoned");
        let store = load_orchestrator_template_store(self.orchestrator_templates_path.as_path())
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to load orchestrator templates: {err:#}"
                ))
            })?;
        Ok(OrchestratorTemplatesResponse {
            templates: store.templates,
        })
    }

    fn get_orchestrator_template(
        &self,
        template_id: &str,
    ) -> Result<OrchestratorTemplateResponse, ApiError> {
        let _guard = self.inner.lock().expect("state mutex poisoned");
        let store = load_orchestrator_template_store(self.orchestrator_templates_path.as_path())
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to load orchestrator templates: {err:#}"
                ))
            })?;
        let template = store
            .templates
            .into_iter()
            .find(|template| template.id == template_id)
            .ok_or_else(|| ApiError::not_found("orchestrator template not found"))?;
        Ok(OrchestratorTemplateResponse { template })
    }

    fn create_orchestrator_template(
        &self,
        request: OrchestratorTemplateDraft,
    ) -> Result<OrchestratorTemplateResponse, ApiError> {
        let normalized = normalize_orchestrator_template_draft(request)?;
        let _guard = self.inner.lock().expect("state mutex poisoned");
        let mut store = load_orchestrator_template_store(self.orchestrator_templates_path.as_path())
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to load orchestrator templates: {err:#}"
                ))
            })?;
        let id = format!("orchestrator-template-{}", store.next_template_number);
        store.next_template_number = store.next_template_number.saturating_add(1).max(1);
        let now = stamp_orchestrator_template_now();
        let template = OrchestratorTemplate {
            id,
            name: normalized.name,
            description: normalized.description,
            project_id: normalized.project_id,
            sessions: normalized.sessions,
            transitions: normalized.transitions,
            created_at: now.clone(),
            updated_at: now,
        };
        store.templates.push(template.clone());
        persist_orchestrator_template_store(self.orchestrator_templates_path.as_path(), &store)
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to persist orchestrator templates: {err:#}"
                ))
            })?;
        Ok(OrchestratorTemplateResponse { template })
    }

    fn update_orchestrator_template(
        &self,
        template_id: &str,
        request: OrchestratorTemplateDraft,
    ) -> Result<OrchestratorTemplateResponse, ApiError> {
        let normalized = normalize_orchestrator_template_draft(request)?;
        let _guard = self.inner.lock().expect("state mutex poisoned");
        let mut store = load_orchestrator_template_store(self.orchestrator_templates_path.as_path())
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to load orchestrator templates: {err:#}"
                ))
            })?;
        let index = store
            .templates
            .iter()
            .position(|template| template.id == template_id)
            .ok_or_else(|| ApiError::not_found("orchestrator template not found"))?;
        let created_at = store.templates[index].created_at.clone();
        let template = OrchestratorTemplate {
            id: template_id.to_owned(),
            name: normalized.name,
            description: normalized.description,
            project_id: normalized.project_id,
            sessions: normalized.sessions,
            transitions: normalized.transitions,
            created_at,
            updated_at: stamp_orchestrator_template_now(),
        };
        store.templates[index] = template.clone();
        persist_orchestrator_template_store(self.orchestrator_templates_path.as_path(), &store)
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to persist orchestrator templates: {err:#}"
                ))
            })?;
        Ok(OrchestratorTemplateResponse { template })
    }

    fn delete_orchestrator_template(
        &self,
        template_id: &str,
    ) -> Result<OrchestratorTemplatesResponse, ApiError> {
        let _guard = self.inner.lock().expect("state mutex poisoned");
        let mut store = load_orchestrator_template_store(self.orchestrator_templates_path.as_path())
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to load orchestrator templates: {err:#}"
                ))
            })?;
        let index = store
            .templates
            .iter()
            .position(|template| template.id == template_id)
            .ok_or_else(|| ApiError::not_found("orchestrator template not found"))?;
        store.templates.remove(index);
        persist_orchestrator_template_store(self.orchestrator_templates_path.as_path(), &store)
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to persist orchestrator templates: {err:#}"
                ))
            })?;
        Ok(OrchestratorTemplatesResponse {
            templates: store.templates,
        })
    }
}

async fn list_orchestrator_templates(
    State(state): State<AppState>,
) -> Result<Json<OrchestratorTemplatesResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_orchestrator_templates()).await?;
    Ok(Json(response))
}

async fn get_orchestrator_template(
    State(state): State<AppState>,
    AxumPath(template_id): AxumPath<String>,
) -> Result<Json<OrchestratorTemplateResponse>, ApiError> {
    let response = run_blocking_api(move || state.get_orchestrator_template(&template_id)).await?;
    Ok(Json(response))
}

async fn create_orchestrator_template(
    State(state): State<AppState>,
    Json(request): Json<OrchestratorTemplateDraft>,
) -> Result<(StatusCode, Json<OrchestratorTemplateResponse>), ApiError> {
    let response = run_blocking_api(move || state.create_orchestrator_template(request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

async fn update_orchestrator_template(
    State(state): State<AppState>,
    AxumPath(template_id): AxumPath<String>,
    Json(request): Json<OrchestratorTemplateDraft>,
) -> Result<Json<OrchestratorTemplateResponse>, ApiError> {
    let response = run_blocking_api(move || state.update_orchestrator_template(&template_id, request)).await?;
    Ok(Json(response))
}

async fn delete_orchestrator_template(
    State(state): State<AppState>,
    AxumPath(template_id): AxumPath<String>,
) -> Result<Json<OrchestratorTemplatesResponse>, ApiError> {
    let response = run_blocking_api(move || state.delete_orchestrator_template(&template_id)).await?;
    Ok(Json(response))
}

fn normalize_orchestrator_template_draft(
    draft: OrchestratorTemplateDraft,
) -> Result<OrchestratorTemplateDraft, ApiError> {
    let name = normalize_required_orchestrator_text(&draft.name, "template name")?;
    let description = draft.description.trim().to_owned();
    let session_count = draft.sessions.len();
    let transition_count = draft.transitions.len();

    if session_count > MAX_ORCHESTRATOR_TEMPLATE_SESSIONS {
        return Err(ApiError::bad_request(format!(
            "orchestrator templates support at most {MAX_ORCHESTRATOR_TEMPLATE_SESSIONS} sessions"
        )));
    }
    if transition_count > MAX_ORCHESTRATOR_TEMPLATE_TRANSITIONS {
        return Err(ApiError::bad_request(format!(
            "orchestrator templates support at most {MAX_ORCHESTRATOR_TEMPLATE_TRANSITIONS} transitions"
        )));
    }

    let mut session_ids = HashSet::new();
    let mut sessions = Vec::with_capacity(session_count);
    for session in draft.sessions {
        let normalized = normalize_orchestrator_session_template(session)?;
        if !session_ids.insert(normalized.id.clone()) {
            return Err(ApiError::bad_request(format!(
                "duplicate session id `{}`",
                normalized.id
            )));
        }
        sessions.push(normalized);
    }

    if sessions.is_empty() {
        return Err(ApiError::bad_request(
            "an orchestrator template needs at least one session",
        ));
    }

    let valid_node_ids = sessions
        .iter()
        .map(|session| session.id.clone())
        .collect::<HashSet<_>>();

    let mut transition_ids = HashSet::new();
    let mut transitions = Vec::with_capacity(transition_count);
    for transition in draft.transitions {
        let normalized = normalize_orchestrator_transition(transition)?;
        if !transition_ids.insert(normalized.id.clone()) {
            return Err(ApiError::bad_request(format!(
                "duplicate transition id `{}`",
                normalized.id
            )));
        }
        if !valid_node_ids.contains(&normalized.from_session_id) {
            return Err(ApiError::bad_request(format!(
                "transition `{}` references unknown source `{}`",
                normalized.id, normalized.from_session_id
            )));
        }
        if !valid_node_ids.contains(&normalized.to_session_id) {
            return Err(ApiError::bad_request(format!(
                "transition `{}` references unknown target `{}`",
                normalized.id, normalized.to_session_id
            )));
        }
        if normalized.from_session_id == normalized.to_session_id {
            return Err(ApiError::bad_request(format!(
                "transition `{}` cannot point to the same session on both ends",
                normalized.id
            )));
        }
        transitions.push(normalized);
    }

    // Cycle detection: build adjacency list and run 3-color DFS.
    {
        let mut adj: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
        for t in &transitions {
            adj.entry(t.from_session_id.as_str())
                .or_default()
                .push(t.to_session_id.as_str());
        }

        // 0 = white (unvisited), 1 = gray (in current path), 2 = black (finished)
        let mut color: std::collections::HashMap<&str, u8> = std::collections::HashMap::new();
        for id in &valid_node_ids {
            color.insert(id.as_str(), 0);
        }

        fn dfs<'a>(
            node: &'a str,
            adj: &std::collections::HashMap<&str, Vec<&'a str>>,
            color: &mut std::collections::HashMap<&'a str, u8>,
        ) -> bool {
            color.insert(node, 1);
            if let Some(neighbors) = adj.get(node) {
                for &next in neighbors {
                    match color.get(next) {
                        Some(1) => return true,
                        Some(0) => {
                            if dfs(next, adj, color) {
                                return true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            color.insert(node, 2);
            false
        }

        for id in &valid_node_ids {
            if color.get(id.as_str()) == Some(&0) {
                if dfs(id.as_str(), &adj, &mut color) {
                    return Err(ApiError::bad_request(
                        "Transition graph contains a cycle: transitions must form a directed acyclic graph (DAG).",
                    ));
                }
            }
        }
    }

    let project_id = draft
        .project_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    Ok(OrchestratorTemplateDraft {
        name,
        description,
        project_id,
        sessions,
        transitions,
    })
}

fn normalize_orchestrator_session_template(
    template: OrchestratorSessionTemplate,
) -> Result<OrchestratorSessionTemplate, ApiError> {
    Ok(OrchestratorSessionTemplate {
        id: normalize_required_orchestrator_text(&template.id, "session id")?,
        name: normalize_required_orchestrator_text(&template.name, "session name")?,
        agent: template.agent,
        model: normalize_optional_orchestrator_text(template.model),
        instructions: template.instructions.trim().to_owned(),
        auto_approve: template.auto_approve,
        position: normalize_orchestrator_position(template.position)?,
    })
}

fn normalize_orchestrator_transition(
    transition: OrchestratorTemplateTransition,
) -> Result<OrchestratorTemplateTransition, ApiError> {
    Ok(OrchestratorTemplateTransition {
        id: normalize_required_orchestrator_text(&transition.id, "transition id")?,
        from_session_id: normalize_required_orchestrator_text(
            &transition.from_session_id,
            "transition source",
        )?,
        to_session_id: normalize_required_orchestrator_text(
            &transition.to_session_id,
            "transition target",
        )?,
        from_anchor: transition.from_anchor,
        to_anchor: transition.to_anchor,
        trigger: transition.trigger,
        result_mode: transition.result_mode,
        prompt_template: normalize_optional_orchestrator_text(transition.prompt_template),
    })
}

fn normalize_required_orchestrator_text(value: &str, label: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(format!("{label} cannot be empty")));
    }
    Ok(trimmed.to_owned())
}

fn normalize_optional_orchestrator_text(value: Option<String>) -> Option<String> {
    value.and_then(|entry| {
        let trimmed = entry.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn normalize_orchestrator_position(
    position: OrchestratorNodePosition,
) -> Result<OrchestratorNodePosition, ApiError> {
    if !position.x.is_finite() || !position.y.is_finite() {
        return Err(ApiError::bad_request(
            "template canvas positions must be finite numbers",
        ));
    }

    Ok(position)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum OrchestratorInstanceStatus {
    #[default]
    Running,
    Paused,
    Stopped,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorSessionInstance {
    template_session_id: String,
    session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_completion_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_delivered_completion_revision: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingTransition {
    id: String,
    transition_id: String,
    source_session_id: String,
    destination_session_id: String,
    completion_revision: u64,
    rendered_prompt: String,
    created_at: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorInstance {
    id: String,
    template_id: String,
    #[serde(default)]
    project_id: String,
    template_snapshot: OrchestratorTemplate,
    #[serde(default)]
    status: OrchestratorInstanceStatus,
    #[serde(default)]
    session_instances: Vec<OrchestratorSessionInstance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_transitions: Vec<PendingTransition>,
    created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateOrchestratorInstanceRequest {
    template_id: String,
    #[serde(default)]
    project_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorInstancesResponse {
    orchestrators: Vec<OrchestratorInstance>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorInstanceResponse {
    orchestrator: OrchestratorInstance,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateOrchestratorInstanceResponse {
    orchestrator: OrchestratorInstance,
    state: StateResponse,
}

impl StateInner {
    fn normalize_orchestrator_instances(&mut self) {
        let available_session_ids = self
            .sessions
            .iter()
            .map(|record| record.session.id.clone())
            .collect::<HashSet<_>>();

        self.orchestrator_instances
            .retain(|instance| !instance.id.trim().is_empty());

        for instance in &mut self.orchestrator_instances {
            instance.template_id = instance.template_id.trim().to_owned();
            instance.project_id = instance.project_id.trim().to_owned();
            instance
                .session_instances
                .retain(|session| available_session_ids.contains(&session.session_id));
            instance.pending_transitions.retain(|transition| {
                available_session_ids.contains(&transition.source_session_id)
                    && available_session_ids.contains(&transition.destination_session_id)
            });
            if instance.status == OrchestratorInstanceStatus::Stopped {
                instance.pending_transitions.clear();
            }
        }

        self.orchestrator_instances
            .retain(|instance| !instance.session_instances.is_empty());
    }
}

impl AppState {
    fn list_orchestrator_instances(&self) -> Result<OrchestratorInstancesResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(OrchestratorInstancesResponse {
            orchestrators: inner.orchestrator_instances.clone(),
        })
    }

    fn get_orchestrator_instance(
        &self,
        instance_id: &str,
    ) -> Result<OrchestratorInstanceResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let orchestrator = inner
            .orchestrator_instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
        Ok(OrchestratorInstanceResponse { orchestrator })
    }

    fn create_orchestrator_instance(
        &self,
        request: CreateOrchestratorInstanceRequest,
    ) -> Result<CreateOrchestratorInstanceResponse, ApiError> {
        let template_id =
            normalize_required_orchestrator_text(&request.template_id, "template id")?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let template = {
            let store =
                load_orchestrator_template_store(self.orchestrator_templates_path.as_path())
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to load orchestrator templates: {err:#}"
                        ))
                    })?;
            store
                .templates
                .into_iter()
                .find(|template| template.id == template_id)
                .ok_or_else(|| ApiError::not_found("orchestrator template not found"))?
        };
        let project_id = request
            .project_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or(template.project_id.as_deref())
            .ok_or_else(|| {
                ApiError::bad_request(
                    "no project specified — set a project on the template or provide one in the request",
                )
            })?;
        let project = inner
            .find_project(project_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("unknown project `{project_id}`")))?;
        if project.remote_id != LOCAL_REMOTE_ID {
            return Err(ApiError::bad_request(
                "runtime orchestrations currently require a local project",
            ));
        }
        for template_session in &template.sessions {
            validate_agent_session_setup(template_session.agent, &project.root_path)
                .map_err(ApiError::bad_request)?;
        }

        let mut session_instances = Vec::with_capacity(template.sessions.len());
        for template_session in &template.sessions {
            let record = inner.create_session(
                template_session.agent,
                Some(template_session.name.clone()),
                project.root_path.clone(),
                Some(project.id.clone()),
                template_session.model.clone(),
            );
            let index = inner
                .find_session_index(&record.session.id)
                .ok_or_else(|| ApiError::internal("new session disappeared during creation"))?;
            apply_orchestrator_template_session_settings(
                &mut inner.sessions[index],
                template_session,
            );
            session_instances.push(OrchestratorSessionInstance {
                template_session_id: template_session.id.clone(),
                session_id: record.session.id.clone(),
                last_completion_revision: None,
                last_delivered_completion_revision: None,
            });
        }

        let orchestrator = OrchestratorInstance {
            id: format!("orchestrator-instance-{}", Uuid::new_v4()),
            template_id: template.id.clone(),
            project_id: project.id.clone(),
            template_snapshot: template,
            status: OrchestratorInstanceStatus::Running,
            session_instances,
            pending_transitions: Vec::new(),
            created_at: stamp_orchestrator_template_now(),
            completed_at: None,
        };
        inner.orchestrator_instances.push(orchestrator.clone());
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist orchestrator instance: {err:#}"))
        })?;
        Ok(CreateOrchestratorInstanceResponse {
            orchestrator,
            state: self.snapshot_from_inner(&inner),
        })
    }

    fn resume_pending_orchestrator_transitions(&self) -> Result<()> {
        loop {
            if !self.accept_next_pending_orchestrator_transition()? {
                break;
            }
        }

        Ok(())
    }

    fn accept_next_pending_orchestrator_transition(
        &self,
    ) -> Result<bool> {
        let dispatch_destination_session_id = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");

            let Some((instance_index, pending)) = inner
                .orchestrator_instances
                .iter()
                .enumerate()
                .find_map(|(index, instance)| {
                    (instance.status == OrchestratorInstanceStatus::Running)
                        .then(|| instance.pending_transitions.first().cloned())
                        .flatten()
                        .map(|pending| (index, pending))
                })
            else {
                return Ok(false);
            };

            let destination_session_index =
                inner.find_session_index(&pending.destination_session_id);
            let destination_template = inner.orchestrator_instances[instance_index]
                .session_instances
                .iter()
                .find(|session| session.session_id == pending.destination_session_id)
                .and_then(|session_instance| {
                    inner.orchestrator_instances[instance_index]
                        .template_snapshot
                        .sessions
                        .iter()
                        .find(|template_session| {
                            template_session.id == session_instance.template_session_id
                        })
                        .cloned()
                });

            let Some(destination_session_index) = destination_session_index else {
                acknowledge_pending_orchestrator_transition(
                    &mut inner,
                    instance_index,
                    &pending,
                );
                self.commit_locked(&mut inner)?;
                return Ok(true);
            };

            let final_prompt = build_orchestrator_destination_prompt(
                &inner.sessions[destination_session_index],
                destination_template
                    .as_ref()
                    .map(|template| template.instructions.as_str())
                    .unwrap_or(""),
                &pending.rendered_prompt,
            );

            if final_prompt.trim().is_empty() {
                acknowledge_pending_orchestrator_transition(
                    &mut inner,
                    instance_index,
                    &pending,
                );
                self.commit_locked(&mut inner)?;
                return Ok(true);
            }

            let should_dispatch_now = !matches!(
                inner.sessions[destination_session_index].session.status,
                SessionStatus::Active | SessionStatus::Approval
            ) && inner.sessions[destination_session_index].queued_prompts.is_empty()
                && !record_has_archived_codex_thread(&inner.sessions[destination_session_index]);
            let message_id = inner.next_message_id();
            queue_prompt_on_record(
                &mut inner.sessions[destination_session_index],
                PendingPrompt {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    text: final_prompt,
                    expanded_text: None,
                },
                Vec::new(),
            );
            acknowledge_pending_orchestrator_transition(
                &mut inner,
                instance_index,
                &pending,
            );
            self.commit_locked(&mut inner)?;

            should_dispatch_now.then(|| pending.destination_session_id.clone())
        };

        if let Some(destination_session_id) = dispatch_destination_session_id {
            let dispatch = self
                .dispatch_next_queued_turn(&destination_session_id)?
                .ok_or_else(|| {
                    anyhow!(
                        "queued orchestrator transition prompt disappeared before dispatch"
                    )
                })?;
            if let Err(err) = deliver_turn_dispatch(self, dispatch) {
                eprintln!(
                    "orchestrator transition warning> failed to dispatch queued prompt for session `{}`: {}",
                    destination_session_id,
                    err.message
                );
            }
        }

        Ok(true)
    }
}

async fn list_orchestrator_instances(
    State(state): State<AppState>,
) -> Result<Json<OrchestratorInstancesResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_orchestrator_instances()).await?;
    Ok(Json(response))
}

async fn get_orchestrator_instance(
    State(state): State<AppState>,
    AxumPath(instance_id): AxumPath<String>,
) -> Result<Json<OrchestratorInstanceResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.get_orchestrator_instance(&instance_id)).await?;
    Ok(Json(response))
}

async fn create_orchestrator_instance(
    State(state): State<AppState>,
    Json(request): Json<CreateOrchestratorInstanceRequest>,
) -> Result<(StatusCode, Json<CreateOrchestratorInstanceResponse>), ApiError> {
    let response = run_blocking_api(move || state.create_orchestrator_instance(request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

fn apply_orchestrator_template_session_settings(
    record: &mut SessionRecord,
    template_session: &OrchestratorSessionTemplate,
) {
    record.session.name = template_session.name.clone();
    if let Some(model) = template_session.model.as_ref() {
        record.session.model = model.clone();
    }

    match record.session.agent {
        agent if agent.supports_codex_prompt_settings() => {
            let approval_policy = if template_session.auto_approve {
                CodexApprovalPolicy::Never
            } else {
                CodexApprovalPolicy::OnRequest
            };
            record.codex_approval_policy = approval_policy;
            record.session.approval_policy = Some(approval_policy);
            record.session.reasoning_effort = Some(record.codex_reasoning_effort);
            record.session.sandbox_mode = Some(record.codex_sandbox_mode);
        }
        agent if agent.supports_claude_approval_mode() => {
            record.session.claude_approval_mode = Some(if template_session.auto_approve {
                ClaudeApprovalMode::AutoApprove
            } else {
                ClaudeApprovalMode::Ask
            });
        }
        agent if agent.supports_cursor_mode() => {
            record.session.cursor_mode = Some(if template_session.auto_approve {
                CursorMode::Agent
            } else {
                CursorMode::Ask
            });
        }
        agent if agent.supports_gemini_approval_mode() => {
            record.session.gemini_approval_mode = Some(if template_session.auto_approve {
                GeminiApprovalMode::AutoEdit
            } else {
                GeminiApprovalMode::Plan
            });
        }
        _ => {}
    }
}

fn acknowledge_pending_orchestrator_transition(
    inner: &mut StateInner,
    instance_index: usize,
    pending: &PendingTransition,
) {
    let instance = &mut inner.orchestrator_instances[instance_index];
    instance
        .pending_transitions
        .retain(|candidate| candidate.id != pending.id);
    update_orchestrator_delivery_cursor(
        instance,
        &pending.source_session_id,
        pending.completion_revision,
    );
}

fn orchestrator_template_session_for_runtime_session(
    inner: &StateInner,
    session_id: &str,
) -> Option<OrchestratorSessionTemplate> {
    inner.orchestrator_instances.iter().find_map(|instance| {
        instance
            .session_instances
            .iter()
            .find(|session_instance| session_instance.session_id == session_id)
            .and_then(|session_instance| {
                instance
                    .template_snapshot
                    .sessions
                    .iter()
                    .find(|template_session| {
                        template_session.id == session_instance.template_session_id
                    })
                    .cloned()
            })
    })
}

fn schedule_orchestrator_transitions_for_completed_session(
    inner: &mut StateInner,
    session_id: &str,
    completion_revision: u64,
) {
    let Some(source_record) = inner
        .find_session_index(session_id)
        .and_then(|index| inner.sessions.get(index))
        .cloned()
    else {
        return;
    };

    for instance in &mut inner.orchestrator_instances {
        if instance.status != OrchestratorInstanceStatus::Running {
            continue;
        }

        let Some(session_instance_index) = instance
            .session_instances
            .iter()
            .position(|candidate| candidate.session_id == session_id)
        else {
            continue;
        };

        instance.session_instances[session_instance_index].last_completion_revision =
            Some(completion_revision);
        let template_session_id = instance.session_instances[session_instance_index]
            .template_session_id
            .clone();
        let source_template = instance
            .template_snapshot
            .sessions
            .iter()
            .find(|session| session.id == template_session_id)
            .cloned();

        for transition in instance.template_snapshot.transitions.iter().filter(|transition| {
            transition.trigger == OrchestratorTransitionTrigger::OnCompletion
                && transition.from_session_id == template_session_id
        }) {
            if instance.pending_transitions.iter().any(|pending| {
                pending.transition_id == transition.id
                    && pending.source_session_id == session_id
                    && pending.completion_revision == completion_revision
            }) {
                continue;
            }

            let Some(destination_session_id) = instance
                .session_instances
                .iter()
                .find(|candidate| candidate.template_session_id == transition.to_session_id)
                .map(|candidate| candidate.session_id.clone())
            else {
                continue;
            };

            let result = build_transition_result_text(&source_record, transition.result_mode);
            let rendered_prompt = render_transition_prompt(
                transition,
                source_template.as_ref(),
                &source_record.session,
                &result,
            );
            instance.pending_transitions.push(PendingTransition {
                id: format!("pending-transition-{}", Uuid::new_v4()),
                transition_id: transition.id.clone(),
                source_session_id: session_id.to_owned(),
                destination_session_id,
                completion_revision,
                rendered_prompt,
                created_at: stamp_orchestrator_template_now(),
            });
        }
    }
}

fn update_orchestrator_delivery_cursor(
    instance: &mut OrchestratorInstance,
    source_session_id: &str,
    completion_revision: u64,
) {
    let still_pending_for_completion = instance.pending_transitions.iter().any(|pending| {
        pending.source_session_id == source_session_id
            && pending.completion_revision == completion_revision
    });
    if still_pending_for_completion {
        return;
    }

    if let Some(session_instance) = instance
        .session_instances
        .iter_mut()
        .find(|candidate| candidate.session_id == source_session_id)
    {
        session_instance.last_delivered_completion_revision = Some(
            session_instance
                .last_delivered_completion_revision
                .unwrap_or(0)
                .max(completion_revision),
        );
    }
}

fn build_transition_result_text(
    record: &SessionRecord,
    mode: OrchestratorTransitionResultMode,
) -> String {
    let current_turn_messages = current_turn_transition_messages(record);
    match mode {
        OrchestratorTransitionResultMode::None => String::new(),
        OrchestratorTransitionResultMode::LastResponse => {
            latest_transition_message_text(current_turn_messages).unwrap_or_default()
        }
        OrchestratorTransitionResultMode::Summary => {
            latest_transition_message_summary(current_turn_messages)
                .unwrap_or_else(|| record.session.preview.trim().to_owned())
        }
        OrchestratorTransitionResultMode::SummaryAndLastResponse => {
            let summary = latest_transition_message_summary(current_turn_messages)
                .unwrap_or_else(|| record.session.preview.trim().to_owned());
            let last_response = latest_transition_message_text(current_turn_messages).unwrap_or_default();
            combine_transition_summary_and_result(&summary, &last_response)
        }
    }
}

fn combine_transition_summary_and_result(summary: &str, last_response: &str) -> String {
    let summary = summary.trim();
    let last_response = last_response.trim();
    match (summary.is_empty(), last_response.is_empty()) {
        (true, true) => String::new(),
        (false, true) => summary.to_owned(),
        (true, false) => last_response.to_owned(),
        (false, false) if summary == last_response => summary.to_owned(),
        (false, false) => format!("Summary:\n{summary}\n\nLast response:\n{last_response}"),
    }
}

fn current_turn_transition_messages(record: &SessionRecord) -> &[Message] {
    record
        .active_turn_start_message_count
        .and_then(|start| record.session.messages.get(start..))
        .unwrap_or(record.session.messages.as_slice())
}

fn latest_transition_message_summary(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(transition_message_summary)
}

fn transition_message_summary(message: &Message) -> Option<String> {
    match message {
        Message::Text {
            author: Author::Assistant,
            text,
            attachments,
            ..
        } => Some(prompt_preview_text(text, attachments)),
        Message::Thinking { title, .. } => Some(make_preview(title)),
        Message::Command {
            command, status, ..
        } => match status {
            CommandStatus::Running => None,
            CommandStatus::Success => Some(format!("Ran {} successfully.", make_preview(command))),
            CommandStatus::Error => Some(format!("Command failed: {}.", make_preview(command))),
        },
        Message::Diff { summary, .. } => Some(make_preview(summary)),
        Message::Markdown { title, .. } => Some(make_preview(title)),
        Message::SubagentResult { title, summary, .. } => {
            let detail = summary.trim();
            if detail.is_empty() {
                Some(make_preview(title))
            } else {
                Some(make_preview(detail))
            }
        }
        Message::ParallelAgents { agents, .. } => Some(parallel_agents_preview_text(agents)),
        Message::Approval { .. }
        | Message::UserInputRequest { .. }
        | Message::McpElicitationRequest { .. }
        | Message::CodexAppRequest { .. }
        | Message::Text {
            author: Author::You, ..
        } => None,
    }
}

fn latest_transition_message_text(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(transition_message_text)
}

fn transition_message_text(message: &Message) -> Option<String> {
    match message {
        Message::Text {
            author: Author::Assistant,
            text,
            expanded_text,
            ..
        } => Some(
            expanded_text
                .as_deref()
                .unwrap_or(text)
                .trim()
                .to_owned(),
        ),
        Message::Markdown { title, markdown, .. } => Some(format!(
            "{}\n\n{}",
            title.trim(),
            markdown.trim()
        ).trim().to_owned()),
        Message::Diff { summary, diff, .. } => Some(format!(
            "{}\n\n{}",
            summary.trim(),
            diff.trim()
        ).trim().to_owned()),
        Message::SubagentResult { title, summary, .. } => Some(format!(
            "{}\n\n{}",
            title.trim(),
            summary.trim()
        ).trim().to_owned()),
        Message::ParallelAgents { agents, .. } => Some(parallel_agents_preview_text(agents)),
        Message::Thinking { title, lines, .. } => {
            let mut parts = vec![title.trim().to_owned()];
            let detail = lines
                .iter()
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !detail.is_empty() {
                parts.push(detail);
            }
            Some(parts.join("\n\n"))
        }
        Message::Command {
            command,
            output,
            status,
            ..
        } => Some(
            format!(
                "Command: {}\nStatus: {}\n\n{}",
                command.trim(),
                status.label(),
                output.trim()
            )
            .trim()
            .to_owned(),
        ),
        Message::Approval { .. }
        | Message::UserInputRequest { .. }
        | Message::McpElicitationRequest { .. }
        | Message::CodexAppRequest { .. }
        | Message::Text {
            author: Author::You, ..
        } => None,
    }
}

fn render_transition_prompt(
    transition: &OrchestratorTemplateTransition,
    source_template: Option<&OrchestratorSessionTemplate>,
    source_session: &Session,
    result: &str,
) -> String {
    let template = transition.prompt_template.as_deref().unwrap_or("{{result}}");
    let rendered = template
        .replace("{{result}}", result)
        .replace(
            "{{sourceSessionId}}",
            source_template
                .map(|session| session.id.as_str())
                .unwrap_or(source_session.id.as_str()),
        )
        .replace(
            "{{sourceSessionName}}",
            source_template
                .map(|session| session.name.as_str())
                .unwrap_or(source_session.name.as_str()),
        )
        .replace("{{transitionId}}", &transition.id);

    if rendered.trim().is_empty() {
        result.trim().to_owned()
    } else {
        rendered.trim().to_owned()
    }
}

fn build_orchestrator_destination_prompt(
    destination_record: &SessionRecord,
    instructions: &str,
    rendered_prompt: &str,
) -> String {
    let prompt = rendered_prompt.trim();
    let instructions = instructions.trim();
    let should_prefix_instructions = destination_record.session.messages.is_empty()
        && destination_record.queued_prompts.is_empty()
        && !instructions.is_empty();

    match (should_prefix_instructions, prompt.is_empty()) {
        (false, false) => prompt.to_owned(),
        (false, true) => String::new(),
        (true, true) => instructions.to_owned(),
        (true, false) => format!(
            "Session instructions:\n{}\n\nPrompt:\n{}",
            instructions, prompt
        ),
    }
}
