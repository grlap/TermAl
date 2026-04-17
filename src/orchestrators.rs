/*
Orchestrator templates and runtime instances
orchestrators.json
  -> template CRUD
  -> normalized graph draft
  -> launch instance
       -> create backing sessions
       -> watch completion signals
       -> evaluate transitions
       -> dispatch downstream prompts
Template definitions are file-backed. Live instances are stored in StateInner so
they advance together with ordinary session lifecycle updates.
*/

/// Resolves orchestrator templates path.
fn resolve_orchestrator_templates_path(default_workdir: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir).join("orchestrators.json")
}

/// Loads orchestrator template store.
fn load_orchestrator_template_store(path: &FsPath) -> Result<OrchestratorTemplateStore> {
    if !path.exists() {
        return Ok(OrchestratorTemplateStore::default());
    }

    let raw = fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    let encoded: Value = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse `{}`", path.display()))?;
    let mut store: OrchestratorTemplateStore =
        serde_json::from_value(encoded).with_context(|| {
            format!(
                "failed to deserialize orchestrator templates from `{}`",
                path.display()
            )
        })?;
    store.normalize();
    Ok(store)
}

/// Persists orchestrator template store.
fn persist_orchestrator_template_store(
    path: &FsPath,
    store: &OrchestratorTemplateStore,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let encoded =
        serde_json::to_vec_pretty(store).context("failed to serialize orchestrator templates")?;
    fs::write(path, encoded).with_context(|| format!("failed to write `{}`", path.display()))
}

/// Stamps orchestrator template now.
fn stamp_orchestrator_template_now() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Returns the default next orchestrator template number.
fn default_next_orchestrator_template_number() -> usize {
    1
}

/// Returns whether false.
fn is_false(value: &bool) -> bool {
    !*value
}

const MAX_ORCHESTRATOR_TEMPLATE_SESSIONS: usize = 50;
const MAX_ORCHESTRATOR_TEMPLATE_TRANSITIONS: usize = 200;

/// Stores orchestrator template.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorTemplateStore {
    #[serde(default = "default_next_orchestrator_template_number")]
    next_template_number: usize,
    #[serde(default)]
    templates: Vec<OrchestratorTemplate>,
}

impl Default for OrchestratorTemplateStore {
    /// Builds the default value.
    fn default() -> Self {
        Self {
            next_template_number: default_next_orchestrator_template_number(),
            templates: Vec::new(),
        }
    }
}

impl OrchestratorTemplateStore {
    /// Handles normalize.
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

/// Defines the orchestrator transition trigger variants.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum OrchestratorTransitionTrigger {
    #[default]
    OnCompletion,
}

/// Enumerates orchestrator transition result modes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum OrchestratorTransitionResultMode {
    None,
    LastResponse,
    Summary,
    SummaryAndLastResponse,
}

/// Enumerates orchestrator session input modes.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum OrchestratorSessionInputMode {
    #[default]
    Queue,
    Consolidate,
}

/// Represents orchestrator node position.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorNodePosition {
    x: f64,
    y: f64,
}

/// Represents the orchestrator session template.
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
    input_mode: OrchestratorSessionInputMode,
    position: OrchestratorNodePosition,
}

/// Represents orchestrator template transition.
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

/// Represents orchestrator template draft.
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

/// Represents the orchestrator template.
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

/// Represents the orchestrator templates response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorTemplatesResponse {
    templates: Vec<OrchestratorTemplate>,
}

/// Represents the orchestrator template response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorTemplateResponse {
    template: OrchestratorTemplate,
}

impl AppState {
    /// Lists orchestrator templates.
    fn list_orchestrator_templates(&self) -> Result<OrchestratorTemplatesResponse, ApiError> {
        let _guard = self
            .orchestrator_templates_lock
            .lock()
            .expect("orchestrator templates mutex poisoned");
        let store = load_orchestrator_template_store(self.orchestrator_templates_path.as_path())
            .map_err(|err| {
                ApiError::internal(format!("failed to load orchestrator templates: {err:#}"))
            })?;
        Ok(OrchestratorTemplatesResponse {
            templates: store.templates,
        })
    }

    /// Gets orchestrator template.
    fn get_orchestrator_template(
        &self,
        template_id: &str,
    ) -> Result<OrchestratorTemplateResponse, ApiError> {
        let _guard = self
            .orchestrator_templates_lock
            .lock()
            .expect("orchestrator templates mutex poisoned");
        let store = load_orchestrator_template_store(self.orchestrator_templates_path.as_path())
            .map_err(|err| {
                ApiError::internal(format!("failed to load orchestrator templates: {err:#}"))
            })?;
        let template = store
            .templates
            .into_iter()
            .find(|template| template.id == template_id)
            .ok_or_else(|| ApiError::not_found("orchestrator template not found"))?;
        Ok(OrchestratorTemplateResponse { template })
    }

    /// Creates orchestrator template.
    fn create_orchestrator_template(
        &self,
        request: OrchestratorTemplateDraft,
    ) -> Result<OrchestratorTemplateResponse, ApiError> {
        let normalized = normalize_orchestrator_template_draft(request)?;
        let _guard = self
            .orchestrator_templates_lock
            .lock()
            .expect("orchestrator templates mutex poisoned");
        let mut store =
            load_orchestrator_template_store(self.orchestrator_templates_path.as_path()).map_err(
                |err| ApiError::internal(format!("failed to load orchestrator templates: {err:#}")),
            )?;
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
                ApiError::internal(format!("failed to persist orchestrator templates: {err:#}"))
            })?;
        Ok(OrchestratorTemplateResponse { template })
    }

    /// Updates orchestrator template.
    fn update_orchestrator_template(
        &self,
        template_id: &str,
        request: OrchestratorTemplateDraft,
    ) -> Result<OrchestratorTemplateResponse, ApiError> {
        let normalized = normalize_orchestrator_template_draft(request)?;
        let _guard = self
            .orchestrator_templates_lock
            .lock()
            .expect("orchestrator templates mutex poisoned");
        let mut store =
            load_orchestrator_template_store(self.orchestrator_templates_path.as_path()).map_err(
                |err| ApiError::internal(format!("failed to load orchestrator templates: {err:#}")),
            )?;
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
                ApiError::internal(format!("failed to persist orchestrator templates: {err:#}"))
            })?;
        Ok(OrchestratorTemplateResponse { template })
    }

    /// Deletes orchestrator template.
    fn delete_orchestrator_template(
        &self,
        template_id: &str,
    ) -> Result<OrchestratorTemplatesResponse, ApiError> {
        let _guard = self
            .orchestrator_templates_lock
            .lock()
            .expect("orchestrator templates mutex poisoned");
        let mut store =
            load_orchestrator_template_store(self.orchestrator_templates_path.as_path()).map_err(
                |err| ApiError::internal(format!("failed to load orchestrator templates: {err:#}")),
            )?;
        let index = store
            .templates
            .iter()
            .position(|template| template.id == template_id)
            .ok_or_else(|| ApiError::not_found("orchestrator template not found"))?;
        store.templates.remove(index);
        persist_orchestrator_template_store(self.orchestrator_templates_path.as_path(), &store)
            .map_err(|err| {
                ApiError::internal(format!("failed to persist orchestrator templates: {err:#}"))
            })?;
        Ok(OrchestratorTemplatesResponse {
            templates: store.templates,
        })
    }
}

/// Lists orchestrator templates.
async fn list_orchestrator_templates(
    State(state): State<AppState>,
) -> Result<Json<OrchestratorTemplatesResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_orchestrator_templates()).await?;
    Ok(Json(response))
}

/// Gets orchestrator template.
async fn get_orchestrator_template(
    State(state): State<AppState>,
    AxumPath(template_id): AxumPath<String>,
) -> Result<Json<OrchestratorTemplateResponse>, ApiError> {
    let response = run_blocking_api(move || state.get_orchestrator_template(&template_id)).await?;
    Ok(Json(response))
}

/// Creates orchestrator template.
async fn create_orchestrator_template(
    State(state): State<AppState>,
    Json(request): Json<OrchestratorTemplateDraft>,
) -> Result<(StatusCode, Json<OrchestratorTemplateResponse>), ApiError> {
    let response = run_blocking_api(move || state.create_orchestrator_template(request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Updates orchestrator template.
async fn update_orchestrator_template(
    State(state): State<AppState>,
    AxumPath(template_id): AxumPath<String>,
    Json(request): Json<OrchestratorTemplateDraft>,
) -> Result<Json<OrchestratorTemplateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.update_orchestrator_template(&template_id, request)).await?;
    Ok(Json(response))
}

/// Deletes orchestrator template.
async fn delete_orchestrator_template(
    State(state): State<AppState>,
    AxumPath(template_id): AxumPath<String>,
) -> Result<Json<OrchestratorTemplatesResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.delete_orchestrator_template(&template_id)).await?;
    Ok(Json(response))
}

/// Normalizes orchestrator template draft.
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
        transitions.push(normalized);
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

/// Builds a runtime template from a draft payload.
fn orchestrator_template_from_draft(
    template_id: &str,
    draft: OrchestratorTemplateDraft,
) -> Result<OrchestratorTemplate, ApiError> {
    let normalized = normalize_orchestrator_template_draft(draft)?;
    let now = stamp_orchestrator_template_now();
    Ok(OrchestratorTemplate {
        id: template_id.to_owned(),
        name: normalized.name,
        description: normalized.description,
        project_id: normalized.project_id,
        sessions: normalized.sessions,
        transitions: normalized.transitions,
        created_at: now.clone(),
        updated_at: now,
    })
}

/// Converts a stored template into a launch payload.
fn orchestrator_template_to_draft(template: &OrchestratorTemplate) -> OrchestratorTemplateDraft {
    OrchestratorTemplateDraft {
        name: template.name.clone(),
        description: template.description.clone(),
        project_id: template.project_id.clone(),
        sessions: template.sessions.clone(),
        transitions: template.transitions.clone(),
    }
}

/// Normalizes orchestrator session template.
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
        input_mode: template.input_mode,
        position: normalize_orchestrator_position(template.position)?,
    })
}

/// Normalizes orchestrator transition.
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

/// Normalizes required orchestrator text.
fn normalize_required_orchestrator_text(value: &str, label: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(format!("{label} cannot be empty")));
    }
    Ok(trimmed.to_owned())
}

/// Normalizes optional orchestrator text.
fn normalize_optional_orchestrator_text(value: Option<String>) -> Option<String> {
    value.and_then(|entry| {
        let trimmed = entry.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

/// Normalizes orchestrator position.
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

/// Enumerates orchestrator instance states.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum OrchestratorInstanceStatus {
    #[default]
    Running,
    Paused,
    Stopped,
}

/// Represents a orchestrator session instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorSessionInstance {
    template_session_id: String,
    session_id: String,
    /// Highest completion revision observed for this runtime session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_completion_revision: Option<u64>,
    /// Highest completion revision whose transitions were fully acknowledged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_delivered_completion_revision: Option<u64>,
}

/// Represents pending transition.
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

/// Represents consolidated pending transitions.
#[derive(Clone, Debug)]
struct ConsolidatedPendingTransitions {
    prompt_pendings: Vec<PendingTransition>,
    acknowledged_pendings: Vec<PendingTransition>,
}

/// Represents consolidated pending inspection.
#[derive(Clone, Debug)]
struct ConsolidatedPendingInspection {
    prompt_pendings: Vec<PendingTransition>,
    acknowledged_pendings: Vec<PendingTransition>,
    missing_source_session_ids: Vec<String>,
}

/// Enumerates pending transition actions.
#[derive(Clone, Debug)]
enum PendingTransitionAction {
    Acknowledge {
        instance_index: usize,
        pendings: Vec<PendingTransition>,
    },
    Deliver {
        destination_session_id: String,
        destination_template: Option<OrchestratorSessionTemplate>,
        instance_index: usize,
        pendings: Vec<PendingTransition>,
        rendered_prompt: String,
    },
}

/// Represents a orchestrator instance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorInstance {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_orchestrator_id: Option<String>,
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
    error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    stop_in_progress: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_session_ids_during_stop: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    stopped_session_ids_during_stop: Vec<String>,
}

/// Represents the create orchestrator instance request payload.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateOrchestratorInstanceRequest {
    template_id: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    template: Option<OrchestratorTemplateDraft>,
}

/// Represents the orchestrator instances response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorInstancesResponse {
    orchestrators: Vec<OrchestratorInstance>,
}

/// Represents the orchestrator instance response payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OrchestratorInstanceResponse {
    orchestrator: OrchestratorInstance,
}

/// Represents the create orchestrator instance response payload.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateOrchestratorInstanceResponse {
    orchestrator: OrchestratorInstance,
    state: StateResponse,
}

impl StateInner {
    /// Normalizes orchestrator instances.
    fn normalize_orchestrator_instances(&mut self) {
        let persisted_non_running_session_ids = HashSet::new();
        self.normalize_orchestrator_instances_with_persisted_non_running(
            &persisted_non_running_session_ids,
        );
    }

    /// Normalizes orchestrator instances with persisted non running.
    fn normalize_orchestrator_instances_with_persisted_non_running(
        &mut self,
        persisted_non_running_session_ids: &HashSet<String>,
    ) {
        let available_session_ids = self
            .sessions
            .iter()
            .map(|record| record.session.id.clone())
            .collect::<HashSet<_>>();

        self.orchestrator_instances
            .retain(|instance| !instance.id.trim().is_empty());

        let mut recovered_stop_in_progress_session_ids = HashSet::new();
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
            if let Some(active_session_ids_during_stop) =
                instance.active_session_ids_during_stop.as_mut()
            {
                active_session_ids_during_stop
                    .retain(|session_id| available_session_ids.contains(session_id));
                active_session_ids_during_stop.sort();
                active_session_ids_during_stop.dedup();
            }
            instance
                .stopped_session_ids_during_stop
                .retain(|session_id| available_session_ids.contains(session_id));
            if instance.stop_in_progress {
                let recovered_stopped_session_ids = instance
                    .stopped_session_ids_during_stop
                    .iter()
                    .cloned()
                    .collect::<HashSet<_>>();
                let stop_reached_all_active_children = instance
                    .active_session_ids_during_stop
                    .as_ref()
                    .is_some_and(|active_session_ids| {
                        active_session_ids.iter().all(|session_id| {
                            recovered_stopped_session_ids.contains(session_id)
                                || persisted_non_running_session_ids.contains(session_id)
                        })
                    });
                if stop_reached_all_active_children {
                    instance.status = OrchestratorInstanceStatus::Stopped;
                    instance.pending_transitions.clear();
                    instance.error_message = None;
                    if instance.completed_at.is_none() {
                        instance.completed_at = Some(stamp_orchestrator_template_now());
                    }
                } else if !recovered_stopped_session_ids.is_empty() {
                    recovered_stop_in_progress_session_ids
                        .extend(recovered_stopped_session_ids.iter().cloned());
                    let dropped_pendings = instance
                        .pending_transitions
                        .iter()
                        .filter(|pending| {
                            recovered_stopped_session_ids.contains(&pending.destination_session_id)
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    for pending in &dropped_pendings {
                        instance
                            .pending_transitions
                            .retain(|candidate| candidate.id != pending.id);
                        update_orchestrator_delivery_cursor(
                            instance,
                            &pending.source_session_id,
                            pending.completion_revision,
                        );
                    }
                }
                instance.stop_in_progress = false;
                instance.active_session_ids_during_stop = None;
                instance.stopped_session_ids_during_stop.clear();
            }
            if instance.status == OrchestratorInstanceStatus::Stopped {
                instance.pending_transitions.clear();
                instance.active_session_ids_during_stop = None;
                instance.stopped_session_ids_during_stop.clear();
            }
        }

        self.orchestrator_instances
            .retain(|instance| !instance.session_instances.is_empty());

        let stopped_session_ids = self
            .orchestrator_instances
            .iter()
            .filter(|instance| instance.status == OrchestratorInstanceStatus::Stopped)
            .flat_map(|instance| {
                instance
                    .session_instances
                    .iter()
                    .map(|session| session.session_id.clone())
            })
            .chain(recovered_stop_in_progress_session_ids)
            .collect::<HashSet<_>>();
        for session_id in stopped_session_ids {
            let Some(session_index) = self.find_session_index(&session_id) else {
                continue;
            };
            // Route through `session_mut_by_index` so the stamp bump
            // makes `collect_persist_delta` pick this up. Going via
            // `&mut self.sessions[session_index]` bypasses stamping,
            // and the delete-session caller that follows up with
            // `commit_locked` would silently drop the queued-prompt
            // clear from SQLite. The load-path caller also reaches
            // here, but re-persisting already-persisted rows on
            // startup is harmless.
            if let Some(record) = self.session_mut_by_index(session_index) {
                clear_stopped_orchestrator_queued_prompts(record);
            }
        }
    }
}

impl AppState {
    /// Lists orchestrator instances.
    fn list_orchestrator_instances(&self) -> Result<OrchestratorInstancesResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(OrchestratorInstancesResponse {
            orchestrators: inner.orchestrator_instances.clone(),
        })
    }

    /// Gets orchestrator instance.
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

    /// Publishes orchestrators updated.
    fn publish_orchestrators_updated(
        &self,
        revision: u64,
        orchestrators: Vec<OrchestratorInstance>,
    ) {
        let sessions = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            referenced_sessions_for_orchestrators(&inner, &orchestrators)
        };
        self.publish_delta(&DeltaEvent::OrchestratorsUpdated {
            revision,
            orchestrators,
            sessions,
        });
    }

    /// Creates orchestrator instance.
    fn create_orchestrator_instance(
        &self,
        request: CreateOrchestratorInstanceRequest,
    ) -> Result<CreateOrchestratorInstanceResponse, ApiError> {
        let template_id =
            normalize_required_orchestrator_text(&request.template_id, "template id")?;

        let template = if let Some(inline_template) = request.template.clone() {
            orchestrator_template_from_draft(&template_id, inline_template)?
        } else {
            let _guard = self
                .orchestrator_templates_lock
                .lock()
                .expect("orchestrator templates mutex poisoned");
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
                    "no project specified; set a project on the template or provide one in the request",
                )
            })?;
        let project = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_project(project_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found(format!("unknown project `{project_id}`")))?
        };
        if project.remote_id != LOCAL_REMOTE_ID {
            return self.create_remote_orchestrator_proxy(&template, &project);
        }
        let (state, orchestrator) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
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
                    inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid"),
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
                remote_id: None,
                remote_orchestrator_id: None,
                template_id: template.id.clone(),
                project_id: project.id.clone(),
                template_snapshot: template,
                status: OrchestratorInstanceStatus::Running,
                session_instances,
                pending_transitions: Vec::new(),
                created_at: stamp_orchestrator_template_now(),
                error_message: None,
                completed_at: None,
                stop_in_progress: false,
                active_session_ids_during_stop: None,
                stopped_session_ids_during_stop: Vec::new(),
            };
            inner.orchestrator_instances.push(orchestrator.clone());
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist orchestrator instance: {err:#}"))
            })?;
            (self.snapshot_from_inner(&inner), orchestrator)
        };
        Ok(CreateOrchestratorInstanceResponse {
            orchestrator,
            state,
        })
    }

    /// Pauses orchestrator instance.
    fn pause_orchestrator_instance(&self, instance_id: &str) -> Result<StateResponse, ApiError> {
        if let Some(target) = self.remote_orchestrator_target(instance_id)? {
            return self.proxy_remote_pause_orchestrator_instance(target);
        }
        let (state, orchestrators) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let instance = inner
                .orchestrator_instances
                .iter_mut()
                .find(|instance| instance.id == instance_id)
                .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
            match instance.status {
                OrchestratorInstanceStatus::Running => {
                    instance.status = OrchestratorInstanceStatus::Paused;
                }
                OrchestratorInstanceStatus::Paused => {
                    return Err(ApiError::conflict("orchestrator is already paused"));
                }
                OrchestratorInstanceStatus::Stopped => {
                    return Err(ApiError::conflict("stopped orchestrators cannot be paused"));
                }
            }
            self.commit_persisted_delta_locked(&mut inner)
                .map_err(|err| {
                    ApiError::internal(format!("failed to persist orchestrator state: {err:#}"))
                })?;
            (
                self.snapshot_from_inner(&inner),
                inner.orchestrator_instances.clone(),
            )
        };
        self.publish_orchestrators_updated(state.revision, orchestrators);
        Ok(state)
    }

    /// Resumes orchestrator instance.
    fn resume_orchestrator_instance(&self, instance_id: &str) -> Result<StateResponse, ApiError> {
        if let Some(target) = self.remote_orchestrator_target(instance_id)? {
            return self.proxy_remote_resume_orchestrator_instance(target);
        }
        let (state, orchestrators) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let instance = inner
                .orchestrator_instances
                .iter_mut()
                .find(|instance| instance.id == instance_id)
                .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
            match instance.status {
                OrchestratorInstanceStatus::Paused => {
                    instance.status = OrchestratorInstanceStatus::Running;
                }
                OrchestratorInstanceStatus::Running => {
                    return Err(ApiError::conflict("orchestrator is already running"));
                }
                OrchestratorInstanceStatus::Stopped => {
                    return Err(ApiError::conflict(
                        "stopped orchestrators cannot be resumed",
                    ));
                }
            }
            self.commit_persisted_delta_locked(&mut inner)
                .map_err(|err| {
                    ApiError::internal(format!("failed to persist orchestrator state: {err:#}"))
                })?;
            (
                self.snapshot_from_inner(&inner),
                inner.orchestrator_instances.clone(),
            )
        };
        self.publish_orchestrators_updated(state.revision, orchestrators);
        self.resume_pending_orchestrator_transitions()
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to resume orchestrator transitions: {err:#}"
                ))
            })?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Begins orchestrator stop.
    fn begin_orchestrator_stop(&self, instance_id: &str) -> Result<(), ApiError> {
        let mut stopping = self
            .stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned");
        if !stopping.insert(instance_id.to_owned()) {
            return Err(ApiError::conflict(
                "orchestrator stop is already in progress",
            ));
        }
        drop(stopping);
        self.stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .insert(instance_id.to_owned(), HashSet::new());

        let result = (|| {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let instance_index = inner
                .orchestrator_instances
                .iter()
                .position(|instance| instance.id == instance_id)
                .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
            if inner.orchestrator_instances[instance_index].status
                == OrchestratorInstanceStatus::Stopped
            {
                return Err(ApiError::conflict("orchestrator is already stopped"));
            }
            let mut active_session_ids = inner.orchestrator_instances[instance_index]
                .session_instances
                .iter()
                .filter_map(|session_instance| {
                    inner
                        .find_session_index(&session_instance.session_id)
                        .and_then(|session_index| {
                            let record = &inner.sessions[session_index];
                            if matches!(
                                record.session.status,
                                SessionStatus::Active | SessionStatus::Approval
                            ) && !matches!(record.runtime, SessionRuntime::None)
                            {
                                Some(session_instance.session_id.clone())
                            } else {
                                None
                            }
                        })
                })
                .collect::<Vec<_>>();
            active_session_ids.sort();
            active_session_ids.dedup();
            inner.orchestrator_instances[instance_index].stop_in_progress = true;
            inner.orchestrator_instances[instance_index].active_session_ids_during_stop =
                Some(active_session_ids);
            inner.orchestrator_instances[instance_index]
                .stopped_session_ids_during_stop
                .clear();
            if let Err(err) = self.persist_internal_locked(&inner) {
                inner.orchestrator_instances[instance_index].stop_in_progress = false;
                inner.orchestrator_instances[instance_index].active_session_ids_during_stop = None;
                return Err(ApiError::internal(format!(
                    "failed to persist orchestrator stop state: {err:#}"
                )));
            }
            Ok(())
        })();
        if let Err(err) = result {
            self.finish_orchestrator_stop(instance_id);
            return Err(err);
        }

        Ok(())
    }

    /// Finishes orchestrator stop.
    fn finish_orchestrator_stop(&self, instance_id: &str) {
        self.stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .remove(instance_id);
        self.stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .remove(instance_id);
    }

    /// Records stopped orchestrator session.
    fn note_stopped_orchestrator_session(&self, instance_id: &str, session_id: &str) {
        self.stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .entry(instance_id.to_owned())
            .or_default()
            .insert(session_id.to_owned());
    }

    /// Returns the stopping orchestrator IDs snapshot.
    fn stopping_orchestrator_ids_snapshot(&self) -> HashSet<String> {
        self.stopping_orchestrator_ids
            .lock()
            .expect("orchestrator stop mutex poisoned")
            .clone()
    }

    /// Returns the stopping orchestrator session IDs snapshot.
    fn stopping_orchestrator_session_ids_snapshot(&self) -> HashMap<String, HashSet<String>> {
        self.stopping_orchestrator_session_ids
            .lock()
            .expect("orchestrator stop session mutex poisoned")
            .clone()
    }

    /// Prunes pending transitions for stopped orchestrator sessions.
    fn prune_pending_transitions_for_stopped_orchestrator_sessions(
        &self,
        instance_id: &str,
    ) -> Result<(), ApiError> {
        let mut stopping_sessions = self.stopping_orchestrator_session_ids_snapshot();
        let mut stopped_session_ids = stopping_sessions.remove(instance_id).unwrap_or_default();

        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let mut changed = false;
            if let Some(instance_index) = inner
                .orchestrator_instances
                .iter()
                .position(|instance| instance.id == instance_id)
            {
                let effective_stopped_session_ids = stopped_session_ids
                    .iter()
                    .cloned()
                    .chain(
                        inner.orchestrator_instances[instance_index]
                            .stopped_session_ids_during_stop
                            .iter()
                            .cloned(),
                    )
                    .collect::<HashSet<_>>();
                if inner.orchestrator_instances[instance_index].stop_in_progress {
                    inner.orchestrator_instances[instance_index].stop_in_progress = false;
                    changed = true;
                }
                if inner.orchestrator_instances[instance_index]
                    .active_session_ids_during_stop
                    .is_some()
                {
                    inner.orchestrator_instances[instance_index].active_session_ids_during_stop =
                        None;
                    changed = true;
                }
                if !inner.orchestrator_instances[instance_index]
                    .stopped_session_ids_during_stop
                    .is_empty()
                {
                    inner.orchestrator_instances[instance_index]
                        .stopped_session_ids_during_stop
                        .clear();
                    changed = true;
                }
                let dropped_pendings = inner.orchestrator_instances[instance_index]
                    .pending_transitions
                    .iter()
                    .filter(|pending| {
                        effective_stopped_session_ids.contains(&pending.destination_session_id)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if !dropped_pendings.is_empty() {
                    for pending in &dropped_pendings {
                        acknowledge_pending_orchestrator_transition(
                            &mut inner,
                            instance_index,
                            pending,
                        );
                    }
                    changed = true;
                }
                stopped_session_ids = effective_stopped_session_ids;
            }

            for session_id in &stopped_session_ids {
                let Some(session_index) = inner.find_session_index(session_id) else {
                    continue;
                };
                let queued_prompt_count = inner.sessions[session_index].queued_prompts.len();
                clear_stopped_orchestrator_queued_prompts(
                    inner
                        .session_mut_by_index(session_index)
                        .expect("session index should be valid"),
                );
                if inner.sessions[session_index].queued_prompts.len() != queued_prompt_count {
                    changed = true;
                }
            }

            if changed {
                self.commit_locked(&mut inner).map_err(|err| {
                    ApiError::internal(format!(
                        "failed to persist orchestrator stop cleanup: {err:#}"
                    ))
                })?;
            }
        }

        Ok(())
    }

    /// Returns whether session not running conflict.
    fn is_session_not_running_conflict(error: &ApiError) -> bool {
        error.status == StatusCode::CONFLICT
            && error.message == SESSION_NOT_RUNNING_CONFLICT_MESSAGE
    }

    /// Stops orchestrator instance.
    fn stop_orchestrator_instance(&self, instance_id: &str) -> Result<StateResponse, ApiError> {
        if let Some(target) = self.remote_orchestrator_target(instance_id)? {
            return self.proxy_remote_stop_orchestrator_instance(target);
        }
        self.begin_orchestrator_stop(instance_id)?;
        let mut resume_after_abort = false;
        let stop_result = (|| {
            let (session_ids, active_session_ids) = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                let instance_index = inner
                    .orchestrator_instances
                    .iter()
                    .position(|instance| instance.id == instance_id)
                    .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
                if inner.orchestrator_instances[instance_index].status
                    == OrchestratorInstanceStatus::Stopped
                {
                    return Err(ApiError::conflict("orchestrator is already stopped"));
                }

                let session_ids = inner.orchestrator_instances[instance_index]
                    .session_instances
                    .iter()
                    .map(|session| session.session_id.clone())
                    .collect::<Vec<_>>();
                let active_session_ids = session_ids
                    .iter()
                    .filter(|session_id| {
                        inner
                            .find_session_index(session_id)
                            .is_some_and(|session_index| {
                                let record = &inner.sessions[session_index];
                                matches!(
                                    record.session.status,
                                    SessionStatus::Active | SessionStatus::Approval
                                ) && !matches!(record.runtime, SessionRuntime::None)
                            })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (session_ids, active_session_ids)
            };
            resume_after_abort = true;

            let mut stop_error = None;
            for session_id in active_session_ids {
                match self.stop_session_with_options(
                    &session_id,
                    StopSessionOptions {
                        dispatch_queued_prompts_on_success: false,
                        orchestrator_stop_instance_id: Some(instance_id.to_owned()),
                    },
                ) {
                    Ok(_) => {}
                    Err(err) => {
                        if Self::is_session_not_running_conflict(&err) {
                            continue;
                        }
                        if stop_error.is_none() {
                            stop_error = Some(err);
                        }
                    }
                }
            }
            if let Some(err) = stop_error {
                return Err(err);
            }

            let (state, orchestrators) = {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                let instance_index = inner
                    .orchestrator_instances
                    .iter()
                    .position(|instance| instance.id == instance_id)
                    .ok_or_else(|| ApiError::not_found("orchestrator instance not found"))?;
                if inner.orchestrator_instances[instance_index].status
                    == OrchestratorInstanceStatus::Stopped
                {
                    return Err(ApiError::conflict("orchestrator is already stopped"));
                }

                {
                    let instance = &mut inner.orchestrator_instances[instance_index];
                    instance.status = OrchestratorInstanceStatus::Stopped;
                    instance.pending_transitions.clear();
                    instance.error_message = None;
                    instance.completed_at = Some(stamp_orchestrator_template_now());
                    instance.stop_in_progress = false;
                    instance.active_session_ids_during_stop = None;
                    instance.stopped_session_ids_during_stop.clear();
                }
                for session_id in &session_ids {
                    let Some(session_index) = inner.find_session_index(session_id) else {
                        continue;
                    };
                    clear_stopped_orchestrator_queued_prompts(
                    inner
                        .session_mut_by_index(session_index)
                        .expect("session index should be valid"),
                );
                }
                self.commit_persisted_delta_locked(&mut inner)
                    .map_err(|err| {
                        ApiError::internal(format!("failed to persist orchestrator state: {err:#}"))
                    })?;
                (
                    self.snapshot_from_inner(&inner),
                    inner.orchestrator_instances.clone(),
                )
            };
            self.publish_orchestrators_updated(state.revision, orchestrators);
            Ok(state)
        })();
        if stop_result.is_err() && resume_after_abort {
            if let Err(err) =
                self.prune_pending_transitions_for_stopped_orchestrator_sessions(instance_id)
            {
                eprintln!(
                    "orchestrator stop warning> failed pruning pending transitions after aborted stop `{}`: {err:#?}",
                    instance_id
                );
            }
        }
        self.finish_orchestrator_stop(instance_id);
        if stop_result.is_err() && resume_after_abort {
            if let Err(err) = self.resume_pending_orchestrator_transitions() {
                eprintln!(
                    "orchestrator stop warning> failed resuming pending transitions after aborted stop `{}`: {err:#}",
                    instance_id
                );
            }
        }
        stop_result
    }

    /// Resumes pending orchestrator transitions.
    fn resume_pending_orchestrator_transitions(&self) -> Result<()> {
        let mut changed = false;
        loop {
            if !self.accept_next_pending_orchestrator_transition()? {
                break;
            }
            changed = true;
        }

        let delta = {
            let stopping_orchestrator_ids = self.stopping_orchestrator_ids_snapshot();
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let deadlocked =
                mark_deadlocked_orchestrator_instances(&mut inner, &stopping_orchestrator_ids);
            if deadlocked {
                self.commit_locked(&mut inner)?;
            }
            if changed || deadlocked {
                Some((inner.revision, inner.orchestrator_instances.clone()))
            } else {
                None
            }
        };
        if let Some((revision, orchestrators)) = delta {
            self.publish_orchestrators_updated(revision, orchestrators);
        }

        Ok(())
    }

    /// Accepts next pending orchestrator transition.
    fn accept_next_pending_orchestrator_transition(&self) -> Result<bool> {
        let stopping_orchestrator_ids = self.stopping_orchestrator_ids_snapshot();
        let dispatch_destination_session_id = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");

            let Some(action) = next_pending_transition_action(&inner, &stopping_orchestrator_ids)
            else {
                return Ok(false);
            };

            match action {
                PendingTransitionAction::Acknowledge {
                    instance_index,
                    pendings,
                } => {
                    for pending in &pendings {
                        acknowledge_pending_orchestrator_transition(
                            &mut inner,
                            instance_index,
                            pending,
                        );
                    }
                    self.commit_locked(&mut inner)?;
                    return Ok(true);
                }
                PendingTransitionAction::Deliver {
                    destination_session_id,
                    destination_template,
                    instance_index,
                    pendings,
                    rendered_prompt,
                } => {
                    let Some(destination_session_index) =
                        inner.find_session_index(&destination_session_id)
                    else {
                        for pending in &pendings {
                            acknowledge_pending_orchestrator_transition(
                                &mut inner,
                                instance_index,
                                pending,
                            );
                        }
                        self.commit_locked(&mut inner)?;
                        return Ok(true);
                    };

                    let final_prompt = build_orchestrator_destination_prompt(
                        &inner.sessions[destination_session_index],
                        destination_template
                            .as_ref()
                            .map(|template| template.instructions.as_str())
                            .unwrap_or(""),
                        &rendered_prompt,
                    );

                    if final_prompt.trim().is_empty() {
                        for pending in &pendings {
                            acknowledge_pending_orchestrator_transition(
                                &mut inner,
                                instance_index,
                                pending,
                            );
                        }
                        self.commit_locked(&mut inner)?;
                        return Ok(true);
                    }

                    let should_dispatch_now = !matches!(
                        inner.sessions[destination_session_index].session.status,
                        SessionStatus::Active | SessionStatus::Approval
                    ) && inner.sessions[destination_session_index]
                        .queued_prompts
                        .is_empty()
                        && !record_has_archived_codex_thread(
                            &inner.sessions[destination_session_index],
                        );
                    let message_id = inner.next_message_id();
                    queue_orchestrator_prompt_on_record(
                        inner
                            .session_mut_by_index(destination_session_index)
                            .expect("session index should be valid"),
                        PendingPrompt {
                            attachments: Vec::new(),
                            id: message_id,
                            timestamp: stamp_now(),
                            text: final_prompt,
                            expanded_text: None,
                        },
                        Vec::new(),
                    );
                    for pending in &pendings {
                        acknowledge_pending_orchestrator_transition(
                            &mut inner,
                            instance_index,
                            pending,
                        );
                    }
                    self.commit_locked(&mut inner)?;

                    should_dispatch_now.then(|| destination_session_id)
                }
            }
        };

        if let Some(destination_session_id) = dispatch_destination_session_id {
            let dispatch = self
                .dispatch_next_queued_turn(&destination_session_id, false)?
                .ok_or_else(|| {
                    anyhow!("queued orchestrator transition prompt disappeared before dispatch")
                })?;
            if let Err(err) = deliver_turn_dispatch(self, dispatch) {
                eprintln!(
                    "orchestrator transition warning> failed to dispatch queued prompt for session `{}`: {}",
                    destination_session_id, err.message
                );
            }
        }

        Ok(true)
    }
}

/// Lists orchestrator instances.
async fn list_orchestrator_instances(
    State(state): State<AppState>,
) -> Result<Json<OrchestratorInstancesResponse>, ApiError> {
    let response = run_blocking_api(move || state.list_orchestrator_instances()).await?;
    Ok(Json(response))
}

/// Gets orchestrator instance.
async fn get_orchestrator_instance(
    State(state): State<AppState>,
    AxumPath(instance_id): AxumPath<String>,
) -> Result<Json<OrchestratorInstanceResponse>, ApiError> {
    let response = run_blocking_api(move || state.get_orchestrator_instance(&instance_id)).await?;
    Ok(Json(response))
}

/// Creates orchestrator instance.
async fn create_orchestrator_instance(
    State(state): State<AppState>,
    Json(request): Json<CreateOrchestratorInstanceRequest>,
) -> Result<(StatusCode, Json<CreateOrchestratorInstanceResponse>), ApiError> {
    let response = run_blocking_api(move || state.create_orchestrator_instance(request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// Pauses orchestrator instance.
async fn pause_orchestrator_instance(
    State(state): State<AppState>,
    AxumPath(instance_id): AxumPath<String>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.pause_orchestrator_instance(&instance_id)).await?;
    Ok(Json(response))
}

/// Resumes orchestrator instance.
async fn resume_orchestrator_instance(
    State(state): State<AppState>,
    AxumPath(instance_id): AxumPath<String>,
) -> Result<Json<StateResponse>, ApiError> {
    let response =
        run_blocking_api(move || state.resume_orchestrator_instance(&instance_id)).await?;
    Ok(Json(response))
}

/// Stops orchestrator instance.
async fn stop_orchestrator_instance(
    State(state): State<AppState>,
    AxumPath(instance_id): AxumPath<String>,
) -> Result<Json<StateResponse>, ApiError> {
    let response = run_blocking_api(move || state.stop_orchestrator_instance(&instance_id)).await?;
    Ok(Json(response))
}
