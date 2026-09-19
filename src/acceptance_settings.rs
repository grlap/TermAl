// Owns project evaluator preferences and operator policy configuration.
// Does not own task evaluations, enablement/doctor, or agent MCP tools.
// New feature boundary alongside acceptance_evaluation_api.rs.

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcceptanceEvaluatorDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_mode: Option<AcceptanceEvaluationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evaluator_agent: Option<Agent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evaluator_model: Option<String>,
}

impl AcceptanceEvaluatorDefaults {
    fn normalize(&mut self) -> Result<(), ApiError> {
        self.evaluator_model = self
            .evaluator_model
            .take()
            .map(|model| model.trim().to_owned());
        self.validate()
    }

    fn validate(&self) -> Result<(), ApiError> {
        if self.default_mode == Some(AcceptanceEvaluationMode::SubAgent) {
            return Err(ApiError::bad_request(
                "This host does not produce sub-agent evaluations; choose Auto, same session or independent session",
            ));
        }
        if self
            .evaluator_agent
            .is_some_and(|agent| !matches!(agent, Agent::Claude | Agent::Codex))
        {
            return Err(ApiError::bad_request(
                "Evaluator agent must be Claude or Codex",
            ));
        }
        if self.evaluator_model.as_ref().is_some_and(|model| {
            model.trim().is_empty() || model.len() > 128 || model.chars().any(char::is_control)
        }) {
            return Err(ApiError::bad_request(
                "Evaluator model must be 1–128 bytes without control characters",
            ));
        }
        if self.evaluator_model.is_some() && self.evaluator_agent.is_none() {
            return Err(ApiError::bad_request(
                "Choose an evaluator agent before setting its model",
            ));
        }
        Ok(())
    }
}

fn auto_acceptance_evaluator_agent(parent: Agent, readiness: &[AgentReadiness]) -> Agent {
    let other = match parent {
        Agent::Claude => Some(Agent::Codex),
        Agent::Codex => Some(Agent::Claude),
        _ => None,
    };
    other
        .filter(|other| {
            readiness.iter().any(|entry| {
                entry.agent == *other
                    && !entry.blocking
                    && entry.status == AgentReadinessStatus::Ready
            })
        })
        .unwrap_or(parent)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceStorePolicy {
    modes: Vec<AcceptanceEvaluationMode>,
    mechanical_basis: String,
    require_source_freshness: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptancePolicySnapshot {
    // POST-only acknowledgement; independent of whether a follow-up read succeeds.
    #[serde(skip_serializing_if = "Option::is_none")]
    write_applied: Option<bool>,
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    reader_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_assurance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    acceptance_evaluation: Option<AcceptanceStorePolicy>,
}

fn parse_acceptance_policy(
    value: Value,
    reader_key: String,
) -> Result<AcceptancePolicySnapshot, ApiError> {
    // Complete-replacement editing fails closed on unknown mode/basis shapes:
    // unlike evaluation requests, this UI must not silently drop future fields.
    #[derive(Deserialize)]
    struct Receipt {
        schema_version: u32,
        policy: String,
        epoch: Option<u64>,
        required_assurance: Option<String>,
        acceptance_evaluation: Option<Policy>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Policy {
        allowed_modes: Vec<AcceptanceEvaluationMode>,
        mechanical_basis: String,
        require_source_freshness: bool,
    }
    let receipt: Receipt = serde_json::from_value(value).map_err(|_| {
        ApiError::from_status(StatusCode::BAD_GATEWAY, "Invalid Engram policy receipt")
    })?;
    if receipt.schema_version != 1
        || !acceptance_policy_hash_valid(&receipt.policy)
        || receipt
            .acceptance_evaluation
            .as_ref()
            .is_some_and(|policy| {
                !matches!(policy.mechanical_basis.as_str(), "asserted" | "observed")
                    || policy
                        .allowed_modes
                        .iter()
                        .enumerate()
                        .any(|(i, mode)| policy.allowed_modes[..i].contains(mode))
            })
    {
        return Err(ApiError::from_status(
            StatusCode::BAD_GATEWAY,
            "Unsupported Engram policy receipt",
        ));
    }
    Ok(AcceptancePolicySnapshot {
        write_applied: None,
        available: true,
        error: None,
        reader_key,
        policy: Some(receipt.policy),
        epoch: receipt.epoch,
        required_assurance: receipt.required_assurance,
        acceptance_evaluation: receipt
            .acceptance_evaluation
            .map(|policy| AcceptanceStorePolicy {
                modes: policy.allowed_modes,
                mechanical_basis: policy.mechanical_basis,
                require_source_freshness: policy.require_source_freshness,
            }),
    })
}

fn acceptance_policy_hash_valid(value: &str) -> bool {
    is_engram_record_id(value)
}

fn read_acceptance_control_policy(
    connection: &EngramConnectionConfig,
    read: impl Fn(
        &EngramConnectionConfig,
        &[String],
        Duration,
    ) -> std::result::Result<Value, EngramTransportError>,
) -> std::result::Result<Value, EngramTransportError> {
    read(
        connection,
        &["control-policy".to_owned(), "show".to_owned()],
        ACCEPTANCE_EVALUATION_POLICY_READ_TIMEOUT,
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateAcceptancePolicyRequest {
    modes: Vec<AcceptanceEvaluationMode>,
    mechanical_basis: String,
    require_source_freshness: bool,
    expected_policy: String,
    reader_key: String,
    reason: String,
    // Browser retains this key with the exact payload when the result is unknown.
    idempotency_key: String,
}

impl UpdateAcceptancePolicyRequest {
    fn args(&self, actor: &str) -> Result<Vec<String>, ApiError> {
        if self.modes.len() > 3
            || self
                .modes
                .iter()
                .enumerate()
                .any(|(i, mode)| self.modes[..i].contains(mode))
            || !matches!(self.mechanical_basis.as_str(), "asserted" | "observed")
            || !acceptance_policy_hash_valid(&self.expected_policy)
            || self.reason.trim().is_empty()
            || self.reason.len() > 1024
            || self.reason.chars().any(char::is_control)
            || !self.idempotency_key.starts_with("termal-policy-")
            || self.idempotency_key.len() > 100
            || !self
                .idempotency_key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(ApiError::bad_request(
                "Invalid acceptance policy change; provide unique modes, a policy hash, a bounded reason and an idempotency key",
            ));
        }
        let mut args = vec![
            "control-policy".to_owned(),
            "set-acceptance-evaluation".to_owned(),
        ];
        if !self.modes.is_empty() {
            args.extend([
                "--modes".to_owned(),
                self.modes
                    .iter()
                    .map(|mode| mode.word().replace('_', "-"))
                    .collect::<Vec<_>>()
                    .join(","),
            ]);
        }
        args.extend([
            "--mechanical-basis".to_owned(),
            self.mechanical_basis.clone(),
        ]);
        if self.require_source_freshness {
            args.push("--require-source-freshness".to_owned());
        }
        args.extend([
            format!("--authorized-by={actor}"),
            format!("--reason={}", self.reason),
            "--idempotency-key".to_owned(),
            self.idempotency_key.clone(),
            "--expected-policy-hash".to_owned(),
            self.expected_policy.clone(),
        ]);
        Ok(args)
    }
}

impl AppState {
    fn acceptance_policy_target(
        &self,
        project_id: &str,
        reader: Option<&str>,
    ) -> Result<WorkReadTarget, ApiError> {
        let (_, target) = self.work_read_snapshot(project_id, reader)?;
        let target = target.map_err(ApiError::conflict)?;
        validate_work_read_target(&target)?;
        Ok(target)
    }

    fn project_acceptance_policy(
        &self,
        project_id: &str,
    ) -> Result<AcceptancePolicySnapshot, ApiError> {
        self.project_acceptance_policy_with_reader(project_id, run_acceptance_evaluation_read)
    }

    fn project_acceptance_policy_with_reader(
        &self,
        project_id: &str,
        read: impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> std::result::Result<Value, EngramTransportError>,
    ) -> Result<AcceptancePolicySnapshot, ApiError> {
        let target = self.acceptance_policy_target(project_id, None)?;
        let receipt = read_acceptance_control_policy(&target.connection, read);
        self.validate_work_read_still_current(project_id, &target)?;
        match receipt {
            Ok(value) => parse_acceptance_policy(value, target.reader_key),
            Err(error) => Ok(AcceptancePolicySnapshot {
                write_applied: None,
                available: false,
                error: Some(format!(
                    "Policy unavailable (older binary or read failure): {error}"
                )),
                reader_key: target.reader_key,
                policy: None,
                epoch: None,
                required_assurance: None,
                acceptance_evaluation: None,
            }),
        }
    }

    // Preferences only: never doctor, reset, revoke, stop a runtime or alter store policy.
    fn update_acceptance_defaults(
        &self,
        project_id: &str,
        mut defaults: AcceptanceEvaluatorDefaults,
    ) -> Result<StateResponse, ApiError> {
        defaults.normalize()?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner.engram_project_resets.contains(project_id) {
            return Err(ApiError::conflict("Engram project reset is in progress"));
        }
        let index = inner
            .projects
            .iter()
            .position(|p| p.id == project_id)
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        if inner.projects[index].remote_id != LOCAL_REMOTE_ID {
            return Err(ApiError::bad_request(
                "Evaluator defaults are local project settings",
            ));
        }
        let previous = inner.projects[index].engram.clone();
        inner.projects[index]
            .engram
            .as_mut()
            .ok_or_else(|| ApiError::conflict("Configure Engram before saving evaluator defaults"))?
            .acceptance_evaluation = Some(defaults);
        if let Err(error) = self.commit_locked(&mut inner) {
            inner.projects[index].engram = previous;
            return Err(ApiError::internal(format!(
                "Could not save evaluator defaults: {error:#}"
            )));
        }
        Ok(self.snapshot_from_inner(&inner))
    }

    fn update_acceptance_policy(
        &self,
        project_id: &str,
        request: UpdateAcceptancePolicyRequest,
    ) -> Result<AcceptancePolicySnapshot, ApiError> {
        self.update_acceptance_policy_with_runner(
            project_id,
            request,
            |connection, args| {
                let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
                // The same ten-second process budget bounds policy reads and writes.
                run_engram_cli_command(
                    connection,
                    &refs,
                    ACCEPTANCE_EVALUATION_POLICY_READ_TIMEOUT,
                    "operator acceptance policy",
                )
            },
            run_acceptance_evaluation_read,
        )
    }

    fn update_acceptance_policy_with_runner(
        &self,
        project_id: &str,
        request: UpdateAcceptancePolicyRequest,
        run: impl Fn(
            &EngramConnectionConfig,
            &[String],
        ) -> std::result::Result<EngramCliOutput, EngramTransportError>,
        read: impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> std::result::Result<Value, EngramTransportError>,
    ) -> Result<AcceptancePolicySnapshot, ApiError> {
        let target = self.acceptance_policy_target(project_id, Some(&request.reader_key))?;
        let args = request.args(&target.connection.actor_id)?;
        self.validate_work_read_still_current(project_id, &target)?;
        // One send only. A transport failure can follow the committed policy change.
        let output = run(&target.connection, &args)
            .map_err(|error| ApiError::from_status(StatusCode::BAD_GATEWAY,
                format!("Policy write outcome unknown; retry the identical change/key or refresh policy: {error}")))?;
        if !output.success {
            let detail = output.failure_detail();
            if output.exit.code() == Some(2)
                && detail.starts_with("error:")
                && detail.contains("Usage:")
                && detail.contains("control-policy set-acceptance-evaluation")
            {
                return Err(ApiError::bad_request(format!(
                    "Policy command was not sent: {detail}"
                )));
            }
            if output.exit.code() == Some(1)
                && detail.starts_with("error: active control policy changed:")
            {
                return Err(ApiError::conflict(detail));
            }
            return Err(ApiError::from_status(
                StatusCode::BAD_GATEWAY,
                format!(
                    "Policy write not confirmed; refresh or retry the identical change/key: {detail}"
                ),
            ));
        }
        // Once the command succeeded, a failed/rotated follow-up read must not
        // recast that known write as a rejected or uncertain attempt.
        let snapshot = self
            .validate_work_read_still_current(project_id, &target)
            .and_then(|()| self.project_acceptance_policy_with_reader(project_id, read));
        let mut snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            Err(error) => AcceptancePolicySnapshot {
                write_applied: None,
                available: false,
                error: Some(format!(
                    "Policy change applied to the selected store, but its current policy could not be refreshed: {}",
                    error.message
                )),
                reader_key: target.reader_key,
                policy: None,
                epoch: None,
                required_assurance: None,
                acceptance_evaluation: None,
            },
        };
        snapshot.write_applied = Some(true);
        Ok(snapshot)
    }
}

#[derive(Clone)]
struct AcceptancePolicyLimiter {
    reads: Arc<tokio::sync::Semaphore>,
    writes: Arc<tokio::sync::Semaphore>,
}

impl AcceptancePolicyLimiter {
    fn new(reads: usize, writes: usize) -> Self {
        Self {
            reads: Arc::new(tokio::sync::Semaphore::new(reads)),
            writes: Arc::new(tokio::sync::Semaphore::new(writes)),
        }
    }
}

async fn get_project_acceptance_policy(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    axum::Extension(limiter): axum::Extension<AcceptancePolicyLimiter>,
) -> Result<Json<AcceptancePolicySnapshot>, ApiError> {
    let permit = limiter.reads.try_acquire_owned().map_err(|_| {
        ApiError::from_status(StatusCode::TOO_MANY_REQUESTS, "Policy reader busy; retry")
    })?;
    run_blocking_api(move || {
        let _permit = permit;
        state.project_acceptance_policy(&id).map(Json)
    })
    .await
}

async fn patch_project_acceptance_defaults(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<AcceptanceEvaluatorDefaults>, JsonRejection>,
) -> Result<Json<StateResponse>, ApiError> {
    let Json(request) = request.map_err(|e| api_json_rejection("Evaluator defaults", e))?;
    run_blocking_api(move || state.update_acceptance_defaults(&id, request).map(Json)).await
}

async fn post_project_acceptance_policy(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::Extension(limiter): axum::Extension<AcceptancePolicyLimiter>,
    request: Result<Json<UpdateAcceptancePolicyRequest>, JsonRejection>,
) -> Result<Json<AcceptancePolicySnapshot>, ApiError> {
    // Browser intent gate, NOT authentication in this local single-user server.
    // No MCP exposure. Reject cross-site and non-browser calls rather than silently
    // treating agent HTTP as an operator click. Local privileged code can spoof HTTP.
    if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) != Some("same-origin")
        || headers
            .get("x-termal-operator-action")
            .and_then(|v| v.to_str().ok())
            != Some("acceptance-policy")
    {
        return Err(ApiError::from_status(
            StatusCode::FORBIDDEN,
            "Use the confirmed operator policy form in project settings",
        ));
    }
    let Json(request) = request.map_err(|e| api_json_rejection("Acceptance policy", e))?;
    let permit = limiter.writes.try_acquire_owned().map_err(|_| {
        ApiError::from_status(StatusCode::TOO_MANY_REQUESTS, "Policy writer busy; retry")
    })?;
    run_blocking_api(move || {
        let _permit = permit;
        state.update_acceptance_policy(&id, request).map(Json)
    })
    .await
}
