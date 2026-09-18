// Host entry points for Engram acceptance evaluations. Owns the parent-side
// request (read the task and the policy, select the mode, spawn the evaluator)
// and the evaluator-only submission (current-child authority, then one
// `engram work evaluate` under the evaluator's own identity). Does not own the
// brief or the submission shape (acceptance_evaluation.rs), delegation
// lifecycle, or any tracker write other than that one evaluate call.

const TERMAL_EVALUATE_ACCEPTANCE_TOOL_NAME: &str = "termal_evaluate_acceptance";
const TERMAL_EVALUATE_ACCEPTANCE_TOOL_DESCRIPTION: &str = "Ask TermAl to produce the acceptance evaluation an Engram task needs before it can be completed. Call this when the tracker refuses completion for a missing acceptance evaluation, or before `done` on a task that has acceptance criteria. Supply the task's workRef; agent (Claude or Codex) and model choose the evaluator and default to this session's agent. TermAl reads the task and the project policy, selects the evaluation mode, and for an independent evaluation spawns a read-only evaluator child that records its verdicts in the tracker under its own identity: wait for it with termal_resume_after_delegations, then read the status or result for what was recorded. When the mode is same_session nothing is spawned and the returned brief tells you how to record the evaluation yourself.";
const TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_TOOL_NAME: &str = "termal_submit_acceptance_evaluation";
const TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_QUALIFIED_TOOL_NAME: &str =
    "mcp__termal-delegation__termal_submit_acceptance_evaluation";
const TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_TOOL_DESCRIPTION: &str = "Record this evaluator's acceptance verdicts in the tracker. TermAl derives the task, evaluation mode, bases, identity, model and attempt key; supply exactly one verdict per acceptance criterion, by its number. A pass must cite at least one evidence locator from your brief. The tool is TermAl control plane, not a workspace mutation: `writePolicy: readOnly` does not prohibit it. A refusal names what to correct; correct it and submit again. Once an evaluation is recorded, further submissions are refused.";

const ACCEPTANCE_EVALUATION_READER_LABEL: &str = "acceptance-evaluation reader";
const ACCEPTANCE_EVALUATION_SUBMIT_LABEL: &str = "acceptance-evaluation submission";
// `control-policy show` reads the policy head only (milliseconds); `doctor`
// carries the same key but audits the whole store first, over a minute on a
// large one, so it is never the per-request read. The admitted set only spares
// a wasted spawn: the tracker enforces the policy when the evaluation is
// recorded. So a failed read (an older binary has no `show`) means "unknown",
// never a refusal.
const ACCEPTANCE_EVALUATION_POLICY_READ_TIMEOUT: Duration = Duration::from_secs(10);

fn acceptance_evaluation_request_tool_definition() -> Value {
    json!({
        "name": TERMAL_EVALUATE_ACCEPTANCE_TOOL_NAME,
        "description": TERMAL_EVALUATE_ACCEPTANCE_TOOL_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["workRef"],
            "properties": {
                "workRef": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_ACCEPTANCE_EVALUATION_WORK_REF_CHARS,
                    "description": "The tracker's reference for the task whose acceptance criteria need an evaluation, for example its short ref."
                },
                "agent": {
                    "type": "string",
                    "enum": ["Codex", "Claude"],
                    "description": "Evaluator agent. Defaults to this session's agent, which must then be Claude or Codex."
                },
                "model": { "type": "string" }
            }
        }
    })
}

fn acceptance_evaluation_submit_tool_definition() -> Value {
    json!({
        "name": TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_TOOL_NAME,
        "description": TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_TOOL_DESCRIPTION,
        "annotations": {
            "title": "Submit acceptance evaluation",
            "readOnlyHint": false,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        },
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["schemaVersion", "verdicts"],
            "properties": {
                "schemaVersion": {
                    "type": "integer",
                    "const": ACCEPTANCE_EVALUATION_SUBMISSION_SCHEMA_VERSION
                },
                "verdicts": {
                    "type": "array",
                    "minItems": 1,
                    "description": "Exactly one entry per acceptance criterion.",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["criterion", "verdict", "rationale"],
                        "properties": {
                            "criterion": {
                                "type": "integer",
                                "minimum": 1,
                                "description": "The criterion's number in your brief."
                            },
                            "verdict": {
                                "type": "string",
                                "enum": ["pass", "fail", "insufficient-evidence", "needs-human"]
                            },
                            "basis": {
                                "type": "string",
                                "enum": ["observed", "asserted", "judgment", "human-required"],
                                "description": "Defaults to judgment."
                            },
                            "rationale": {
                                "type": "string",
                                "minLength": 1,
                                "maxLength": MAX_ACCEPTANCE_EVALUATION_RATIONALE_CHARS,
                                "description": "One line: what you checked and what you found."
                            },
                            "evidence": {
                                "type": "array",
                                "maxItems": MAX_ACCEPTANCE_EVALUATION_EVIDENCE_PER_CRITERION,
                                "items": { "type": "string", "pattern": "^[a-f0-9]{8,64}$" },
                                "description": "Evidence locators from your brief. A pass needs at least one."
                            }
                        }
                    }
                }
            }
        }
    })
}

static ACCEPTANCE_EVALUATION_REQUEST_PERMITS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));
static ACCEPTANCE_EVALUATION_SUBMIT_PERMITS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestAcceptanceEvaluationRequest {
    work_ref: String,
    #[serde(default)]
    agent: Option<Agent>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
enum AcceptanceEvaluationRequestResponse {
    /// The ordinary delegation creation response plus what was selected.
    Spawned {
        #[serde(flatten)]
        delegation: DelegationResponse,
        mode: AcceptanceEvaluationMode,
        work_ref: String,
    },
    SameSession {
        mode: AcceptanceEvaluationMode,
        work_ref: String,
        acceptance_basis: i64,
        evidence_basis: i64,
        brief: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationSubmitResponse {
    schema_version: u32,
    delegation_id: String,
    work_ref: String,
    mode: AcceptanceEvaluationMode,
    attempt_key: String,
    recorded_at: String,
    receipt: Value,
}

/// What an admitted submission runs against. The receipt is later stored only
/// on the delegation that still carries this exact attempt key.
#[derive(Clone, Debug)]
struct AcceptanceEvaluationSubmitAuthority {
    delegation_id: String,
    target: DelegationAcceptanceEvaluation,
}

// One locked authority boundary for both permission admission and execution.
fn acceptance_evaluation_submit_authority_locked(
    inner: &StateInner,
    child: &str,
) -> Result<AcceptanceEvaluationSubmitAuthority, ApiError> {
    let index = inner
        .find_delegation_index_by_child_session_id(child)
        .ok_or_else(|| {
            ApiError::conflict("acceptance evaluation submission requires an active evaluator child")
        })?;
    let delegation = &inner.delegations[index];
    let record = inner
        .find_session_index(child)
        .map(|i| &inner.sessions[i])
        .ok_or_else(|| ApiError::not_found("evaluator child no longer exists"))?;
    if delegation.mode != DelegationMode::Evaluator
        || delegation.status != DelegationStatus::Running
        || !matches!(delegation.write_policy, DelegationWritePolicy::ReadOnly)
        || record.hidden
        || !record.is_local_session()
        || record.session.parent_delegation_id.as_deref() != Some(delegation.id.as_str())
    {
        return Err(ApiError::conflict(
            "acceptance evaluation submission requires an active local read-only evaluator",
        ));
    }
    let target = delegation.acceptance_evaluation.as_ref().ok_or_else(|| {
        ApiError::conflict("this evaluator delegation has no evaluation target")
    })?;
    if target.outcome.is_some() {
        return Err(ApiError::conflict(
            "this evaluator already recorded its evaluation; an evaluator records exactly one",
        ));
    }
    Ok(AcceptanceEvaluationSubmitAuthority {
        delegation_id: delegation.id.clone(),
        target: target.clone(),
    })
}

/// The Work view's host connection for `session`'s project: the operator-
/// validated binary, home, declaration and store, with the host reader's
/// identity. Reads under it never register a tracker session.
fn acceptance_evaluation_host_target_locked(
    inner: &StateInner,
    session_id: &str,
) -> Result<WorkReadTarget, ApiError> {
    let project = engram_project_for_session_locked(inner, session_id).ok_or_else(|| {
        ApiError::conflict(
            "this session has no project; Engram is configured per project, so there is no tracker to evaluate against",
        )
    })?;
    work_host_reader(inner, project).map_err(ApiError::conflict)
}

fn acceptance_evaluation_transport_error(operation: &str, error: EngramTransportError) -> ApiError {
    ApiError::bad_gateway(format!("{operation}: {error}"))
}

// Conservative Windows command-line bound, as the Work reader applies it.
fn validate_acceptance_evaluation_command_size(
    connection: &EngramConnectionConfig,
    args: &[String],
) -> Result<(), ApiError> {
    let fixed = [
        connection.binary_path.to_string_lossy().into_owned(),
        connection.project_file.to_string_lossy().into_owned(),
        connection.home.to_string_lossy().into_owned(),
    ];
    let units: usize = fixed
        .iter()
        .chain(args)
        .map(|arg| 2 * arg.encode_utf16().count() + 3)
        .sum();
    if units > 30_000 {
        return Err(ApiError::bad_request(
            "the evaluation is too large for one submission; shorten the rationales",
        ));
    }
    Ok(())
}

fn run_acceptance_evaluation_read(
    connection: &EngramConnectionConfig,
    args: &[String],
    timeout: Duration,
) -> std::result::Result<Value, EngramTransportError> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_engram_json_command_with_lock_retry(
        connection,
        &args,
        timeout,
        ACCEPTANCE_EVALUATION_READER_LABEL,
    )
}

// A locked store is retried once: the attempt key makes a resend a replay.
fn run_acceptance_evaluation_submit(
    connection: &EngramConnectionConfig,
    args: &[String],
    timeout: Duration,
) -> std::result::Result<EngramCliOutput, EngramTransportError> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_engram_cli_command_with_lock_retry(
        connection,
        &args,
        timeout,
        ACCEPTANCE_EVALUATION_SUBMIT_LABEL,
    )
}

impl AppState {
    fn request_acceptance_evaluation(
        &self,
        parent_session_id: &str,
        request: RequestAcceptanceEvaluationRequest,
    ) -> Result<AcceptanceEvaluationRequestResponse, ApiError> {
        self.request_acceptance_evaluation_with_runner(
            parent_session_id,
            request,
            run_acceptance_evaluation_read,
        )
    }

    // Internal reader boundary for deterministic tests. It is not a
    // caller-controlled executable or API field.
    fn request_acceptance_evaluation_with_runner(
        &self,
        parent_session_id: &str,
        request: RequestAcceptanceEvaluationRequest,
        read: impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> std::result::Result<Value, EngramTransportError>,
    ) -> Result<AcceptanceEvaluationRequestResponse, ApiError> {
        let work_ref = request.work_ref.trim().to_owned();
        validate_acceptance_evaluation_work_ref(&work_ref)?;
        let (target, parent_workdir) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(parent_session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            (
                acceptance_evaluation_host_target_locked(&inner, parent_session_id)?,
                inner.sessions[index].session.workdir.clone(),
            )
        };
        // The ref is caller text: never hand it to a shell shim or to a store
        // other than the one the operator established.
        validate_work_read_target(&target)?;
        let connection = &target.connection;
        let mut show_args = vec![
            "work".to_owned(),
            "--actor-id".to_owned(),
            connection.actor_id.clone(),
            "--session-id".to_owned(),
            connection.session_id.clone(),
        ];
        if let Some(actor_context) = connection.actor_context.as_ref() {
            show_args.extend(["--actor-context".to_owned(), actor_context.clone()]);
        }
        // Two reads: the CLI refuses `--full` together with the evidence
        // windows, and the windowed read clips long criteria.
        let mut full_args = show_args.clone();
        show_args.extend(
            ["show", work_ref.as_str(), "--notes", "--gates", "--json"].map(str::to_owned),
        );
        full_args.extend(["show", work_ref.as_str(), "--full", "--json"].map(str::to_owned));
        let mut show = read(connection, &show_args, ENGRAM_WORK_BINDING_COMMAND_TIMEOUT)
            .map_err(|e| acceptance_evaluation_transport_error("engram work show", e))?;
        // Older evidence sits behind the byte-bounded first window. A failed
        // continuation only shortens the brief; it never fails the request.
        let mut older_pages = Vec::new();
        let mut collected = acceptance_evidence_page_len(&show);
        let mut after = acceptance_evidence_continuation(&show);
        while let Some(token) = after.take() {
            if older_pages.len() + 1 >= MAX_ACCEPTANCE_EVIDENCE_PAGES
                || collected >= MAX_ACCEPTANCE_BRIEF_EVIDENCE_ENTRIES
            {
                break;
            }
            let mut page_args = show_args.clone();
            let json_flag = page_args.pop();
            page_args.extend(["--after".to_owned(), token]);
            page_args.extend(json_flag);
            match read(connection, &page_args, ENGRAM_WORK_BINDING_COMMAND_TIMEOUT) {
                Ok(page) => {
                    collected += acceptance_evidence_page_len(&page);
                    after = acceptance_evidence_continuation(&page);
                    older_pages.push(page);
                }
                Err(error) => {
                    eprintln!(
                        "acceptance evaluation> older evidence of `{work_ref}` was not read; the brief lists the newest entries only: {error}"
                    );
                }
            }
        }
        merge_acceptance_evidence_pages(&mut show, older_pages);
        let full = read(connection, &full_args, ENGRAM_WORK_BINDING_COMMAND_TIMEOUT)
            .map_err(|e| acceptance_evaluation_transport_error("engram work show --full", e))?;
        let task = parse_acceptance_evaluation_task(show, full)?;

        let policy_args = ["control-policy", "show"].map(str::to_owned);
        let admitted = match read(
            connection,
            &policy_args,
            ACCEPTANCE_EVALUATION_POLICY_READ_TIMEOUT,
        ) {
            Ok(policy) => acceptance_evaluation_admitted_modes(&policy),
            Err(error) => {
                eprintln!(
                    "acceptance evaluation> admitted modes unknown for `{work_ref}`; the tracker still enforces its policy: {error}"
                );
                None
            }
        };
        let mode = select_acceptance_evaluation_mode(task.pinned_mode.as_deref(), admitted.as_deref())
            .map_err(ApiError::conflict)?;

        match mode {
            AcceptanceEvaluationMode::IndependentSession => {
                let prompt = build_acceptance_evaluator_prompt(
                    &task,
                    &parent_workdir,
                    MAX_DELEGATION_PROMPT_BYTES,
                )?;
                let delegation = self.create_delegation_with_evaluation_target(
                    parent_session_id,
                    CreateDelegationRequest {
                        prompt,
                        title: Some(format!("Acceptance evaluation: {}", task.work_ref)),
                        cwd: None,
                        agent: request.agent,
                        model: request.model,
                        mode: Some(DelegationMode::Evaluator),
                        write_policy: Some(DelegationWritePolicy::ReadOnly),
                    },
                    Some(task.target_seed(mode)),
                )?;
                Ok(AcceptanceEvaluationRequestResponse::Spawned {
                    delegation,
                    mode,
                    work_ref: task.work_ref,
                })
            }
            AcceptanceEvaluationMode::SameSession => {
                Ok(AcceptanceEvaluationRequestResponse::SameSession {
                    mode,
                    brief: build_same_session_acceptance_brief(&task),
                    acceptance_basis: task.acceptance_basis,
                    evidence_basis: task.evidence_basis,
                    work_ref: task.work_ref,
                })
            }
            AcceptanceEvaluationMode::SubAgent => Err(ApiError::from_status(
                StatusCode::NOT_IMPLEMENTED,
                "the task or its policy requires a sub_agent evaluation, which this host does not produce yet",
            )),
        }
    }

    /// Authority plus everything the tracker call needs, read under one lock:
    /// the project's connection carrying the CHILD's session, seat and context.
    /// A submission that does not cover the target's criteria is refused here,
    /// as the evaluator's own error, before the project is even consulted.
    fn acceptance_evaluation_submit_context(
        &self,
        child: &str,
        request: &SubmitAcceptanceEvaluationRequest,
    ) -> Result<
        (
            AcceptanceEvaluationSubmitAuthority,
            WorkReadTarget,
            Option<String>,
        ),
        ApiError,
    > {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let authority = acceptance_evaluation_submit_authority_locked(&inner, child)?;
        request
            .validate_coverage(authority.target.criteria_count)
            .map_err(ApiError::bad_request)?;
        let record = inner
            .find_session_index(child)
            .map(|i| &inner.sessions[i])
            .ok_or_else(|| ApiError::not_found("evaluator child no longer exists"))?;
        let mut target = acceptance_evaluation_host_target_locked(&inner, child)?;
        let (actor_id, actor_context) =
            engram_runtime_actor_identity(&inner.preferences.engram.developer_name, record);
        target.connection.actor_id = actor_id;
        target.connection.actor_context = actor_context;
        target.connection.session_id = child.to_owned();
        let model = acceptance_evaluator_model_flag(record.session.agent, &record.session.model);
        Ok((authority, target, model))
    }

    fn submit_acceptance_evaluation(
        &self,
        child: &str,
        request: SubmitAcceptanceEvaluationRequest,
    ) -> Result<AcceptanceEvaluationSubmitResponse, ApiError> {
        self.submit_acceptance_evaluation_with_runner(
            child,
            request,
            run_acceptance_evaluation_submit,
        )
    }

    // Internal executor boundary for deterministic identity and refusal tests.
    // It is not a caller-controlled executable or API field.
    fn submit_acceptance_evaluation_with_runner(
        &self,
        child: &str,
        request: SubmitAcceptanceEvaluationRequest,
        run: impl FnOnce(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> std::result::Result<EngramCliOutput, EngramTransportError>,
    ) -> Result<AcceptanceEvaluationSubmitResponse, ApiError> {
        request.validate_shape().map_err(ApiError::bad_request)?;
        let (authority, target, model) =
            self.acceptance_evaluation_submit_context(child, &request)?;
        // Rationales are model text: never hand them to a shell shim.
        validate_work_read_target(&target)?;
        let args = acceptance_evaluation_cli_args(
            &target.connection,
            &authority.target,
            &request,
            model.as_deref(),
        );
        validate_acceptance_evaluation_command_size(&target.connection, &args)?;

        let output = run(
            &target.connection,
            &args,
            ENGRAM_WORK_BINDING_COMMAND_TIMEOUT,
        )
        .map_err(|e| acceptance_evaluation_transport_error("engram work evaluate", e))?;
        if !output.success {
            let detail = truncate_chars(
                &acceptance_evaluation_refusal_text(&output.failure_detail()),
                4_000,
            );
            if output.reports_locked_store() {
                return Err(ApiError::bad_gateway(format!(
                    "engram work evaluate: the store stayed locked and nothing was recorded; submit the same verdicts again: {detail}"
                )));
            }
            // The tracker's own words: the evaluator corrects and resubmits.
            return Err(ApiError::conflict(if detail.is_empty() {
                format!(
                    "Engram refused the evaluation without a reason ({}); nothing was recorded",
                    output.status
                )
            } else {
                format!("Engram refused the evaluation; nothing was recorded: {detail}")
            }));
        }
        let receipt = serde_json::from_slice::<Value>(&output.stdout).map_err(|e| {
            ApiError::bad_gateway(format!(
                "engram work evaluate: the receipt was unreadable ({e}); submit the same verdicts again to recover it"
            ))
        })?;

        // The tracker has recorded it: keep the receipt wherever this exact
        // attempt still exists, whatever happened to the child meanwhile.
        let recorded_at = stamp_now();
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let stored = inner
            .find_delegation_index(&authority.delegation_id)
            .filter(|index| {
                inner.delegations[*index]
                    .acceptance_evaluation
                    .as_ref()
                    .is_some_and(|current| {
                        current.attempt_key == authority.target.attempt_key
                            && current.outcome.is_none()
                    })
            });
        if let Some(index) = stored {
            if let Some(current) = inner.delegations[index].acceptance_evaluation.as_mut() {
                current.outcome = Some(DelegationAcceptanceEvaluationOutcome {
                    receipt: receipt.clone(),
                    recorded_at: recorded_at.clone(),
                });
            }
            inner.mark_delegation_mutated(index);
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!(
                    "the tracker recorded the evaluation, but TermAl failed to persist its receipt: {err:#}"
                ))
            })?;
        }
        drop(inner);
        Ok(AcceptanceEvaluationSubmitResponse {
            schema_version: ACCEPTANCE_EVALUATION_SUBMISSION_SCHEMA_VERSION,
            delegation_id: authority.delegation_id,
            work_ref: authority.target.work_ref,
            mode: authority.target.mode,
            attempt_key: authority.target.attempt_key,
            recorded_at,
            receipt,
        })
    }
}

fn acquire_acceptance_evaluation_permit(
    permits: Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    permits.try_acquire_owned().map_err(|_| {
        ApiError::from_status(
            StatusCode::TOO_MANY_REQUESTS,
            "acceptance evaluation busy; retry the same request",
        )
    })
}

async fn request_session_acceptance_evaluation(
    AxumPath(parent_session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<RequestAcceptanceEvaluationRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<AcceptanceEvaluationRequestResponse>), ApiError> {
    let Json(request) =
        request.map_err(|e| api_json_rejection("acceptance evaluation request", e))?;
    let permit =
        acquire_acceptance_evaluation_permit(ACCEPTANCE_EVALUATION_REQUEST_PERMITS.clone())?;
    // The owned permit stays with the blocking operation, even if HTTP drops.
    run_blocking_api(move || {
        let _permit = permit;
        let response = state.request_acceptance_evaluation(&parent_session_id, request)?;
        let status = match response {
            AcceptanceEvaluationRequestResponse::Spawned { .. } => StatusCode::CREATED,
            AcceptanceEvaluationRequestResponse::SameSession { .. } => StatusCode::OK,
        };
        Ok((status, Json(response)))
    })
    .await
}

async fn submit_session_acceptance_evaluation(
    AxumPath(child): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<SubmitAcceptanceEvaluationRequest>, JsonRejection>,
) -> Result<Json<AcceptanceEvaluationSubmitResponse>, ApiError> {
    run_acceptance_evaluation_submit_request(
        child,
        state,
        request,
        ACCEPTANCE_EVALUATION_SUBMIT_PERMITS.clone(),
        |state, child, request| state.submit_acceptance_evaluation(child, request),
    )
    .await
}

async fn run_acceptance_evaluation_submit_request(
    child: String,
    state: AppState,
    request: Result<Json<SubmitAcceptanceEvaluationRequest>, JsonRejection>,
    permits: Arc<tokio::sync::Semaphore>,
    submit: impl FnOnce(
        AppState,
        &str,
        SubmitAcceptanceEvaluationRequest,
    ) -> Result<AcceptanceEvaluationSubmitResponse, ApiError>
    + Send
    + 'static,
) -> Result<Json<AcceptanceEvaluationSubmitResponse>, ApiError> {
    let Json(request) =
        request.map_err(|e| api_json_rejection("acceptance evaluation submission", e))?;
    let permit = acquire_acceptance_evaluation_permit(permits)?;
    run_blocking_api(move || {
        let _permit = permit;
        submit(state, &child, request).map(Json)
    })
    .await
}
