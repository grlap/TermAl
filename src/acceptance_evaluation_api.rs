// Host entry points for Engram acceptance evaluations. Owns the parent-side
// request (read the task and the policy within one budget, select the mode,
// spawn the one evaluator a task may have) and the evaluator-only submission
// (current-child authority, the persisted submission state, then `engram work
// evaluate` under the evaluator's own identity, resent once when its outcome
// is unknown). Does not own the brief or the submission shape
// (acceptance_evaluation.rs), delegation lifecycle, or any tracker write other
// than that evaluate call.

const TERMAL_EVALUATE_ACCEPTANCE_TOOL_NAME: &str = "termal_evaluate_acceptance";
const TERMAL_EVALUATE_ACCEPTANCE_TOOL_DESCRIPTION: &str = "Ask TermAl to produce the acceptance evaluation an Engram task needs before it can be completed. Call this when the tracker refuses completion for a missing acceptance evaluation, or before `done` on a task that has acceptance criteria. Supply the task's workRef; agent (Claude or Codex) and model choose the evaluator and default to this session's agent. TermAl reads the task and the project policy, selects the evaluation mode, and for an independent evaluation spawns a read-only evaluator child that records its verdicts in the tracker under its own identity: wait for it with termal_resume_after_delegations, then read the status or result for what was recorded. One evaluator runs per task: a request made while one is running is refused and names that delegation and its parent session, so wait on it or ask that session. When the mode is same_session nothing is spawned and the returned brief tells you how to record the evaluation yourself.";
const TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_TOOL_NAME: &str = "termal_submit_acceptance_evaluation";
const TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_QUALIFIED_TOOL_NAME: &str =
    "mcp__termal-delegation__termal_submit_acceptance_evaluation";
const TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_TOOL_DESCRIPTION: &str = "Record this evaluator's acceptance verdicts in the tracker. TermAl derives the task, evaluation mode, bases, identity, model and attempt key; supply exactly one verdict per acceptance criterion, by its number. A pass must cite at least one evidence locator from your brief. The tool is TermAl control plane, not a workspace mutation: `writePolicy: readOnly` does not prohibit it. A refusal names what to correct; correct it and submit again. When the answer says the write outcome is unknown, submit exactly the same verdicts again: the tracker replays them, and changed verdicts are refused because only a receipt resolves it. If the answer says nothing was sent, submit again. If it says a submission is already in progress, let that one answer, then submit the same verdicts again. If a refusal adds that an earlier send's outcome is unknown, do not change the verdicts: finish and report that, and the parent reads the task. Once an evaluation is recorded, further submissions are refused.";

const ACCEPTANCE_EVALUATION_READER_LABEL: &str = "acceptance-evaluation reader";
const ACCEPTANCE_EVALUATION_SUBMIT_LABEL: &str = "acceptance-evaluation submission";
// `control-policy show` reads the policy head only (milliseconds); `doctor`
// carries the same key but audits the whole store first, over a minute on a
// large one, so it is never the per-request read. The admitted set only spares
// a wasted spawn: the tracker enforces the policy when the evaluation is
// recorded. So a failed read (an older binary has no `show`) means "unknown",
// never a refusal.
const ACCEPTANCE_EVALUATION_POLICY_READ_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPTANCE_EVALUATION_STORE_CHANGED_ERROR: &str = "the project's tracker store changed since this evaluation was requested; request a new evaluation";

/// One tracker call through the lock-retry runner: two command timeouts and
/// the delay between them.
fn acceptance_evaluation_call_worst_case(timeout: Duration) -> Duration {
    timeout * 2 + ENGRAM_WORK_BINDING_LOCK_RETRY_DELAY
}

/// Worst-case tracker time of one request: the windowed and the complete task
/// read, every evidence continuation page, and the policy read. The request
/// path's own deadline and the bridge's HTTP allowance are both this, so the
/// bridge never gives up on a request the backend is still serving.
fn acceptance_evaluation_request_tracker_budget() -> Duration {
    let task_reads = 2 + (MAX_ACCEPTANCE_EVIDENCE_PAGES as u32 - 1);
    acceptance_evaluation_call_worst_case(ENGRAM_WORK_BINDING_COMMAND_TIMEOUT) * task_reads
        + acceptance_evaluation_call_worst_case(ACCEPTANCE_EVALUATION_POLICY_READ_TIMEOUT)
}

/// What must still fit after a continuation page: that page, the complete
/// task read and the policy read. Paging stops once the deadline cannot fund
/// it; the reads that decide the request are never the ones cut.
fn acceptance_evaluation_paging_reserve() -> Duration {
    acceptance_evaluation_call_worst_case(ENGRAM_WORK_BINDING_COMMAND_TIMEOUT) * 2
        + acceptance_evaluation_call_worst_case(ACCEPTANCE_EVALUATION_POLICY_READ_TIMEOUT)
}

/// How long a submission waits for the writer to acknowledge one state.
#[cfg(not(test))]
const ACCEPTANCE_EVALUATION_PERSIST_ACK_TIMEOUT: Duration = Duration::from_secs(5);
// Tests step a writer by hand under full-suite load, so there the limit only
// diagnoses a deadlock, as every other fixture wait does.
#[cfg(test)]
const ACCEPTANCE_EVALUATION_PERSIST_ACK_TIMEOUT: Duration = TEST_PHASE_DEADLOCK_GUARD;
/// How often a waiting submission looks whether the record it asked about is
/// still the record there is.
const ACCEPTANCE_EVALUATION_PERSIST_ACK_RECHECK: Duration = Duration::from_millis(250);
/// How many records one acknowledgement may ask about within its deadline.
const ACCEPTANCE_EVALUATION_PERSIST_ACK_ATTEMPTS: usize = 3;

/// Worst-case time of one submission beyond the ordinary request: the
/// evaluate call and, when its outcome is unknown, the one identical resend,
/// plus the two acknowledged states (`pending` before, the outcome after).
fn acceptance_evaluation_submit_budget() -> Duration {
    acceptance_evaluation_call_worst_case(ENGRAM_WORK_BINDING_COMMAND_TIMEOUT) * 2
        + ACCEPTANCE_EVALUATION_PERSIST_ACK_TIMEOUT * 2
}

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
        /// An earlier evaluator of the same task ended with an open write.
        #[serde(skip_serializing_if = "Option::is_none")]
        notice: Option<String>,
    },
    SameSession {
        mode: AcceptanceEvaluationMode,
        work_ref: String,
        acceptance_basis: i64,
        evidence_basis: i64,
        brief: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        notice: Option<String>,
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
    /// The tracker's raw receipt, which only this response carries: the
    /// record keeps a bounded extract. Text when it had to be cut.
    receipt: Value,
    #[serde(skip_serializing_if = "is_false")]
    receipt_truncated: bool,
}

/// What an admitted submission runs against. The receipt is later stored only
/// on the delegation that still carries this exact attempt key.
#[derive(Clone, Debug)]
struct AcceptanceEvaluationSubmitAuthority {
    delegation_id: String,
    target: DelegationAcceptanceEvaluation,
}

/// What an admitted submission is to run, and against what.
struct AcceptanceEvaluationBegun {
    /// What an earlier request left open with these same verdicts: to be
    /// treated as an unknown first send of this request. `None` when this
    /// request entered from `none` and wrote `pending` itself.
    uncertain_at_entry: Option<String>,
    payload_digest: String,
    /// The argument list to run: this request's own, or the open write's.
    original: AcceptanceEvaluationOpenWrite,
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
    // An open write still admits the child. Recorded in memory is not final
    // while its submit call holds the guard: let the narrowly scoped tool
    // reach execution's retryable in-progress response, never an agent prompt.
    if matches!(
        target.submission,
        AcceptanceEvaluationSubmission::Recorded { .. }
    ) && !inner.acceptance_evaluation_submissions_in_flight.contains(&delegation.id) {
        return Err(ApiError::conflict(ACCEPTANCE_EVALUATION_ALREADY_RECORDED_ERROR));
    }
    Ok(AcceptanceEvaluationSubmitAuthority {
        delegation_id: delegation.id.clone(),
        target: target.clone(),
    })
}

const ACCEPTANCE_EVALUATION_ALREADY_RECORDED_ERROR: &str =
    "this evaluator already recorded its evaluation; an evaluator records exactly one";

/// The one submission an evaluator delegation may have in progress, from its
/// admission through the tracker run to the last durability acknowledgement.
/// A second one overlapping it could settle on what it saw at its own start
/// (`none` over the other's open write) or answer from a `recorded` the other
/// has not had acknowledged. Taken under the state lock by the admission that
/// creates it; released on every way out, a panic in the tracker runner
/// included. Must not be dropped while the state lock is held.
struct AcceptanceEvaluationSubmissionInFlight {
    state: AppState,
    delegation_id: String,
}

impl AcceptanceEvaluationSubmissionInFlight {
    /// `inner` is the locked state of `state`. `None` when the delegation
    /// already has a submission in progress.
    fn admit_locked(state: &AppState, inner: &mut StateInner, delegation_id: &str) -> Option<Self> {
        inner
            .acceptance_evaluation_submissions_in_flight
            .insert(delegation_id.to_owned())
            .then(|| Self {
                state: state.clone(),
                delegation_id: delegation_id.to_owned(),
            })
    }
}

impl Drop for AcceptanceEvaluationSubmissionInFlight {
    fn drop(&mut self) {
        // A poisoned lock must not keep the evaluator locked out for good.
        let mut inner = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner
            .acceptance_evaluation_submissions_in_flight
            .remove(&self.delegation_id);
    }
}

/// Runs under the lock that creates the evaluator delegation, so neither a
/// settings change during the off-lock reads nor a second concurrent request
/// can slip between this check and the spawn.
fn acceptance_evaluation_spawn_admission_locked(
    inner: &StateInner,
    parent_session_id: &str,
    seed: &AcceptanceEvaluationTargetSeed,
) -> Result<(), ApiError> {
    let current = acceptance_evaluation_host_target_locked(inner, parent_session_id)?;
    if current.store != seed.store {
        return Err(ApiError::conflict(ACCEPTANCE_EVALUATION_STORE_CHANGED_ERROR));
    }
    refuse_second_active_acceptance_evaluator_locked(inner, &seed.store, &seed.work_ref, None)
}

/// Each evaluator has its own attempt key, so the tracker cannot tell a second
/// one judging the same task from a new intent: the host allows one queued or
/// running evaluator per store and work ref. Every way to make an evaluator
/// active takes this check under the lock that makes it so.
fn refuse_second_active_acceptance_evaluator_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    work_ref: &str,
    except_delegation_id: Option<&str>,
) -> Result<(), ApiError> {
    let active = inner.delegations.iter().find(|delegation| {
        delegation.mode == DelegationMode::Evaluator
            && Some(delegation.id.as_str()) != except_delegation_id
            && matches!(
                delegation.status,
                DelegationStatus::Queued | DelegationStatus::Running
            )
            && delegation
                .acceptance_evaluation
                .as_ref()
                .is_some_and(|target| target.judges(store, work_ref))
    });
    let Some(active) = active else {
        return Ok(());
    };
    Err(ApiError::conflict(format!(
        "an acceptance evaluation of `{}` is already running: delegation `{}`, requested by session `{}`. Wait for that delegation or ask that session; do not start another",
        acceptance_brief_text(work_ref, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS),
        active.id,
        active.parent_session_id
    )))
}

/// A follow-up rearms a finished delegation, which for an evaluator is a
/// second way to get an active one. No-op for every other delegation.
fn acceptance_evaluation_followup_admission_locked(
    inner: &StateInner,
    delegation: &DelegationRecord,
) -> Result<(), ApiError> {
    if delegation.mode != DelegationMode::Evaluator {
        return Ok(());
    }
    let Some((target, store)) = delegation
        .acceptance_evaluation
        .as_ref()
        .and_then(|target| Some((target, target.store.as_ref()?)))
    else {
        return Ok(());
    };
    refuse_second_active_acceptance_evaluator_locked(
        inner,
        store,
        &target.work_ref,
        Some(&delegation.id),
    )
}

/// A finished evaluator whose write is still open does not block a new one,
/// but the requester must know the tracker may already hold its verdict.
fn acceptance_evaluation_open_write_notice_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    work_ref: &str,
    except_delegation_id: Option<&str>,
) -> Option<String> {
    let earlier = inner.delegations.iter().find(|delegation| {
        delegation.mode == DelegationMode::Evaluator
            && Some(delegation.id.as_str()) != except_delegation_id
            && delegation.acceptance_evaluation.as_ref().is_some_and(|target| {
                target.judges(store, work_ref)
                    && matches!(
                        target.submission,
                        AcceptanceEvaluationSubmission::Pending { .. }
                            | AcceptanceEvaluationSubmission::Unconfirmed { .. }
                    )
            })
    })?;
    Some(format!(
        "An earlier evaluator of this task (delegation `{}`) ended with its write outcome unknown: the tracker may already hold its verdict. Read the task before relying on this evaluation alone.",
        earlier.id
    ))
}

impl DelegationAcceptanceEvaluation {
    /// The same task in the same tracker store.
    fn judges(&self, store: &EngramAuthorityStoreKey, work_ref: &str) -> bool {
        self.work_ref == work_ref && self.store.as_ref() == Some(store)
    }
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
        self.request_acceptance_evaluation_until(
            parent_session_id,
            request,
            read,
            std::time::Instant::now() + acceptance_evaluation_request_tracker_budget(),
            std::time::Instant::now,
        )
    }

    // `deadline` bounds the tracker reads and `now` is the clock it is read
    // against; both are parameters so paging against a spent deadline is
    // tested without waiting.
    fn request_acceptance_evaluation_until(
        &self,
        parent_session_id: &str,
        request: RequestAcceptanceEvaluationRequest,
        read: impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> std::result::Result<Value, EngramTransportError>,
        deadline: std::time::Instant,
        now: impl Fn() -> std::time::Instant,
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
            // A slow store shortens the brief; it never outruns the caller's
            // HTTP allowance or starves the reads that decide the request.
            if now() + acceptance_evaluation_paging_reserve() > deadline {
                eprintln!(
                    "acceptance evaluation> the read budget for `{work_ref}` is spent; the brief lists the evidence read so far"
                );
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
                    MAX_ACCEPTANCE_BRIEF_BYTES,
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
                    // Creation re-resolves the store and refuses a second
                    // active evaluator of this task, under its own lock.
                    Some(task.target_seed(mode, target.store.clone())),
                )?;
                let notice = self.acceptance_evaluation_open_write_notice(
                    &target.store,
                    &task.work_ref,
                    Some(&delegation.delegation.id),
                );
                Ok(AcceptanceEvaluationRequestResponse::Spawned {
                    delegation,
                    mode,
                    work_ref: task.work_ref,
                    notice,
                })
            }
            AcceptanceEvaluationMode::SameSession => {
                Ok(AcceptanceEvaluationRequestResponse::SameSession {
                    mode,
                    brief: build_same_session_acceptance_brief(&task, MAX_ACCEPTANCE_BRIEF_BYTES)?,
                    acceptance_basis: task.acceptance_basis,
                    evidence_basis: task.evidence_basis,
                    notice: self.acceptance_evaluation_open_write_notice(
                        &target.store,
                        &task.work_ref,
                        None,
                    ),
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
    /// The same critical section that admits the submission marks it in
    /// flight; the returned guard holds that until the caller is done.
    fn acceptance_evaluation_submit_context(
        &self,
        child: &str,
        request: &SubmitAcceptanceEvaluationRequest,
    ) -> Result<
        (
            AcceptanceEvaluationSubmitAuthority,
            WorkReadTarget,
            Option<String>,
            AcceptanceEvaluationSubmissionInFlight,
        ),
        ApiError,
    > {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        // Before anything is read off the record: while a submission is in
        // flight its state in memory is not settled, an unacknowledged
        // `recorded` included, and must not decide this caller's answer.
        let in_progress = || {
            ApiError::conflict(
                "a submission for this evaluator is already in progress; submit the same verdicts again when it has answered",
            )
        };
        if inner
            .find_delegation_index_by_child_session_id(child)
            .is_some_and(|index| {
                inner
                    .acceptance_evaluation_submissions_in_flight
                    .contains(&inner.delegations[index].id)
            })
        {
            return Err(in_progress());
        }
        let authority = acceptance_evaluation_submit_authority_locked(&inner, child)?;
        request
            .validate_coverage(authority.target.criteria_count)
            .map_err(ApiError::bad_request)?;
        let record = inner
            .find_session_index(child)
            .map(|i| &inner.sessions[i])
            .ok_or_else(|| ApiError::not_found("evaluator child no longer exists"))?;
        // The child's own project decides where the write goes, so it is the
        // child's store that must still be the one the brief was read from:
        // the same ref and bases can exist in another store.
        let mut target = acceptance_evaluation_host_target_locked(&inner, child)?;
        match authority.target.store.as_ref() {
            Some(store) if *store == target.store => {}
            Some(_) => return Err(ApiError::conflict(ACCEPTANCE_EVALUATION_STORE_CHANGED_ERROR)),
            None => {
                return Err(ApiError::conflict(
                    "this evaluation does not say which tracker store it was read from; request a new evaluation",
                ));
            }
        }
        let (actor_id, actor_context) =
            engram_runtime_actor_identity(&inner.preferences.engram.developer_name, record);
        target.connection.actor_id = actor_id;
        target.connection.actor_context = actor_context;
        target.connection.session_id = child.to_owned();
        let model = acceptance_evaluator_model_flag(record.session.agent, &record.session.model);
        // Last, after everything that can refuse: the guard relocks the state
        // when it drops, so none may exist before this lock is released.
        let in_flight = AcceptanceEvaluationSubmissionInFlight::admit_locked(
            self,
            &mut inner,
            &authority.delegation_id,
        )
        .ok_or_else(in_progress)?;
        Ok((authority, target, model, in_flight))
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
    // It is not a caller-controlled executable or API field. It may be called
    // twice: an unknown outcome is resolved by one identical resend.
    fn submit_acceptance_evaluation_with_runner(
        &self,
        child: &str,
        request: SubmitAcceptanceEvaluationRequest,
        run: impl Fn(
            &EngramConnectionConfig,
            &[String],
            Duration,
        ) -> std::result::Result<EngramCliOutput, EngramTransportError>,
    ) -> Result<AcceptanceEvaluationSubmitResponse, ApiError> {
        request.validate_shape().map_err(ApiError::bad_request)?;
        // Held to the end of this call, the last acknowledgement included.
        let (authority, target, model, _in_flight) =
            self.acceptance_evaluation_submit_context(child, &request)?;
        // Rationales are model text: never hand them to a shell shim.
        validate_work_read_target(&target)?;
        let args = acceptance_evaluation_cli_args(
            &target.connection,
            &authority.target,
            &request,
            model.as_deref(),
        )?;
        // An open write replays its stored command, not this freshly built
        // candidate. Host identity/model growth must not reject a valid replay.
        if matches!(authority.target.submission, AcceptanceEvaluationSubmission::None) {
            validate_acceptance_evaluation_command_size(&target.connection, &args)?;
        }

        // Acknowledged durable before the tracker runs: whatever happens
        // next, a restart never reads this record as "nothing was sent".
        let AcceptanceEvaluationBegun {
            uncertain_at_entry,
            payload_digest,
            original,
        } = self.begin_acceptance_evaluation_submission(
            &authority,
            AcceptanceEvaluationOpenWrite {
                verdicts_digest: acceptance_evaluation_payload_digest(
                    &acceptance_evaluation_verdict_args(&request),
                ),
                args,
            },
        )?;
        // What runs is what the open write was sent as. For an earlier
        // request's write that is its own argument list, under the actor it
        // named, whatever the developer name or the model has become since.
        let mut connection = target.connection.clone();
        if uncertain_at_entry.is_some() {
            let Some((actor_id, actor_context)) =
                acceptance_evaluation_args_identity(&original.args)
            else {
                return Err(ApiError::conflict(
                    "the open submission's original command cannot be read back, so it cannot be sent again; finish and report this; the parent must read the task in the tracker",
                ));
            };
            connection.actor_id = actor_id;
            connection.actor_context = actor_context;
            validate_acceptance_evaluation_command_size(&connection, &original.args).map_err(
                |_| {
                    ApiError::conflict(
                        "the open submission's original command no longer fits this host's command line, so it cannot be sent again; finish and report this; the parent must read the task in the tracker",
                    )
                },
            )?;
        }

        let send = || {
            classify_acceptance_evaluation_run(run(
                &connection,
                &original.args,
                ENGRAM_WORK_BINDING_COMMAND_TIMEOUT,
            ))
        };
        // An earlier request's open write counts as this request's unknown
        // first send, so the one send here is already its identical resend.
        let mut uncertain = uncertain_at_entry;
        let mut outcome = send();
        if uncertain.is_none() {
            if let AcceptanceEvaluationRunOutcome::Unknown(reason) = &outcome {
                uncertain = Some(reason.clone());
                outcome = send();
            }
        }
        let receipt = match (outcome, uncertain) {
            // Only a receipt ends uncertainty.
            (AcceptanceEvaluationRunOutcome::Receipt(receipt), _) => receipt,
            // From `none`, a run that recorded nothing leaves nothing open:
            // the evaluator may correct its verdicts.
            (AcceptanceEvaluationRunOutcome::Refused(words), None) => {
                self.withdraw_own_acceptance_evaluation_pending(&authority, &payload_digest);
                // The tracker's own words: the evaluator corrects and resubmits.
                return Err(ApiError::conflict(format!(
                    "Engram refused the evaluation; nothing was recorded: {words}"
                )));
            }
            (AcceptanceEvaluationRunOutcome::Locked(detail), None) => {
                self.withdraw_own_acceptance_evaluation_pending(&authority, &payload_digest);
                return Err(ApiError::bad_gateway(format!(
                    "engram work evaluate: the store stayed locked and nothing was recorded; submit the same verdicts again: {detail}"
                )));
            }
            (AcceptanceEvaluationRunOutcome::NeverStarted(detail), None) => {
                self.withdraw_own_acceptance_evaluation_pending(&authority, &payload_digest);
                return Err(ApiError::bad_gateway(format!(
                    "{detail}: the tracker never ran, so nothing was sent; submit again"
                )));
            }
            // With an open write, nothing short of a receipt proves where it
            // stands, a refused resend included: see the run caveat in
            // docs/features/engram-host-adapter.md.
            (AcceptanceEvaluationRunOutcome::Refused(words), Some(earlier)) => {
                self.keep_acceptance_evaluation_unconfirmed(
                    &authority,
                    payload_digest,
                    original,
                    Some(&earlier),
                    &format!("the identical resend was refused: {words}"),
                );
                return Err(ApiError::conflict(format!(
                    "Engram refused the identical resend: {words}. An earlier send of these verdicts has an unknown outcome, so they cannot be changed: finish and report this; the parent must read the task in the tracker"
                )));
            }
            (
                AcceptanceEvaluationRunOutcome::Locked(detail)
                | AcceptanceEvaluationRunOutcome::NeverStarted(detail),
                earlier @ Some(_),
            )
            | (AcceptanceEvaluationRunOutcome::Unknown(detail), earlier) => {
                let reason = self.keep_acceptance_evaluation_unconfirmed(
                    &authority,
                    payload_digest,
                    original,
                    earlier.as_deref(),
                    &detail,
                );
                return Err(ApiError::bad_gateway(format!(
                    "engram work evaluate: the write outcome is unknown; submit the same verdicts again ({reason})"
                )));
            }
        };

        // The tracker holds it: keep the extract wherever this exact attempt
        // still exists, whatever happened to the child meanwhile. Success is
        // answered only once that is acknowledged durable.
        let recorded_at = stamp_now();
        let recorded = AcceptanceEvaluationSubmission::Recorded {
            receipt: acceptance_evaluation_receipt_extract(&receipt),
            recorded_at: recorded_at.clone(),
        };
        if let Err(err) = self.settle_open_acceptance_evaluation_submission(&authority, recorded) {
            return Err(ApiError::internal(format!(
                "the tracker recorded the evaluation, but TermAl could not confirm that it persisted that ({err}); submit the same verdicts again: the tracker replays them and the receipt is recovered"
            )));
        }
        let (receipt, receipt_truncated) = bounded_acceptance_evaluation_receipt(receipt);
        Ok(AcceptanceEvaluationSubmitResponse {
            schema_version: ACCEPTANCE_EVALUATION_SUBMISSION_SCHEMA_VERSION,
            delegation_id: authority.delegation_id,
            work_ref: authority.target.work_ref,
            mode: authority.target.mode,
            attempt_key: authority.target.attempt_key,
            recorded_at,
            receipt,
            receipt_truncated,
        })
    }

    fn acceptance_evaluation_open_write_notice(
        &self,
        store: &EngramAuthorityStoreKey,
        work_ref: &str,
        except_delegation_id: Option<&str>,
    ) -> Option<String> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        acceptance_evaluation_open_write_notice_locked(&inner, store, work_ref, except_delegation_id)
    }

    /// Makes an open write durable, or refuses. `Ok` says what runs: for a
    /// request entering from `none`, its own argument list, now `pending`; for
    /// one that finds an earlier request's write open with the same verdicts
    /// (`pending` or `unconfirmed`), that write's original argument list, which
    /// this request must treat as an unknown first send. Nothing may reach the
    /// tracker unless this returned `Ok`: a restart must find the open write,
    /// never an absent state beside a tracker write.
    fn begin_acceptance_evaluation_submission(
        &self,
        authority: &AcceptanceEvaluationSubmitAuthority,
        current: AcceptanceEvaluationOpenWrite,
    ) -> Result<AcceptanceEvaluationBegun, ApiError> {
        let current_digest = acceptance_evaluation_payload_digest(&current.args);
        let (written, begun, dispatch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index =
                acceptance_evaluation_attempt_index_locked(&inner, authority).ok_or_else(|| {
                    ApiError::conflict(
                        "this evaluator's delegation no longer carries its evaluation target",
                    )
                })?;
            let Some(target) = inner.delegations[index].acceptance_evaluation.as_mut() else {
                return Err(ApiError::conflict(
                    "this evaluator delegation has no evaluation target",
                ));
            };
            let open = match &target.submission {
                AcceptanceEvaluationSubmission::None => None,
                AcceptanceEvaluationSubmission::Recorded { .. } => {
                    return Err(ApiError::conflict(ACCEPTANCE_EVALUATION_ALREADY_RECORDED_ERROR));
                }
                AcceptanceEvaluationSubmission::Pending {
                    payload_digest,
                    original,
                    ..
                } => Some((
                    payload_digest,
                    original,
                    "an earlier submission of these verdicts was started and its outcome never learned"
                        .to_owned(),
                )),
                AcceptanceEvaluationSubmission::Unconfirmed {
                    payload_digest,
                    original,
                    reason,
                    ..
                } => Some((payload_digest, original, reason.clone())),
            };
            let begun = match open {
                None => AcceptanceEvaluationBegun {
                    uncertain_at_entry: None,
                    payload_digest: current_digest,
                    original: current,
                },
                Some((open_digest, original, reason)) => {
                    // The evaluator's part decides: the host's part of the
                    // list may have drifted since the write was sent. A write
                    // kept without it is compared as a whole list.
                    let same_verdicts = if original.verdicts_digest.is_empty() {
                        *open_digest == current_digest
                    } else {
                        original.verdicts_digest == current.verdicts_digest
                    };
                    if !same_verdicts {
                        // The tracker may hold the open write: only its replay is safe.
                        return Err(ApiError::conflict(
                            "an earlier submission's outcome is unknown; submit exactly the same verdicts to resolve it",
                        ));
                    }
                    AcceptanceEvaluationBegun {
                        uncertain_at_entry: Some(reason),
                        payload_digest: open_digest.clone(),
                        original: if original.args.is_empty() {
                            current
                        } else {
                            original.clone()
                        },
                    }
                }
            };
            // An open write is left exactly as it is.
            let dispatch = if begun.uncertain_at_entry.is_none() {
                target.submission = AcceptanceEvaluationSubmission::Pending {
                    payload_digest: begun.payload_digest.clone(),
                    started_at: stamp_now(),
                    original: begun.original.clone(),
                };
                inner.mark_delegation_mutated(index);
                match self.commit_locked_with_persist_dispatch(&mut inner) {
                    Ok((_, dispatch)) => Some(dispatch),
                    Err(err) => {
                        if let Some(target) = inner.delegations[index].acceptance_evaluation.as_mut()
                        {
                            target.submission = AcceptanceEvaluationSubmission::None;
                        }
                        return Err(ApiError::internal(format!(
                            "failed to persist the submission before running the tracker, so nothing was sent; submit the same verdicts again: {err:#}"
                        )));
                    }
                }
            } else {
                None
            };
            (inner.delegations[index].clone(), begun, dispatch)
        };
        // A commit only wakes the writer; a synchronous one already wrote.
        // An open write found at entry is acknowledged again all the same.
        if dispatch == Some(PersistDispatch::Synchronous) {
            return Ok(begun);
        }
        if let Err(reason) =
            self.confirm_acceptance_evaluation_submission_durable(authority, &written)
        {
            // Nothing was sent, so `none` is the truth again where this
            // request's own `pending` still stands. An earlier open write is
            // not this request's to withdraw.
            if begun.uncertain_at_entry.is_none() {
                self.withdraw_own_acceptance_evaluation_pending(authority, &begun.payload_digest);
            }
            return Err(ApiError::internal(format!(
                "the submission was not acknowledged as persisted before running the tracker ({reason}), so nothing was sent; submit the same verdicts again"
            )));
        }
        Ok(begun)
    }

    /// `commit_locked` only wakes the writer. This returns once SQLite holds
    /// the delegation with the submission state `written` carries, or says why
    /// that is not known. The acknowledgement is for an exact record, so a
    /// record that moved on for an unrelated reason (a cancel, a status
    /// refresh) would never match: while its submission state is still the
    /// one written, the record as it now stands is asked for instead, a
    /// bounded number of times within the one deadline. It blocks, so the
    /// caller must not hold the state lock.
    fn confirm_acceptance_evaluation_submission_durable(
        &self,
        authority: &AcceptanceEvaluationSubmitAuthority,
        written: &DelegationRecord,
    ) -> std::result::Result<(), String> {
        let deadline = std::time::Instant::now() + ACCEPTANCE_EVALUATION_PERSIST_ACK_TIMEOUT;
        let written_submission = written
            .acceptance_evaluation
            .as_ref()
            .map(|target| &target.submission);
        let mut expected = written.clone();
        let mut attempts = 1;
        loop {
            let (fence, waiter) = PersistFence::new(
                PersistFenceTarget::Delegation(Box::new(expected.clone())),
                deadline,
            );
            if self
                .persist_tx
                .send(PersistRequest::Fence(Box::new(fence)))
                .is_err()
            {
                // No writer (shutdown): write here, as a commit without one does.
                let inner = self.inner.lock().expect("state mutex poisoned");
                if !inner.delegations.iter().any(|record| *record == expected) {
                    return Err("the delegation changed before it was persisted".to_owned());
                }
                return self
                    .persist_internal_locked(&inner)
                    .map_err(|error| format!("{error:#}"));
            }
            // Same content: keep waiting. Other content: ask again below.
            let current = loop {
                let recheck = std::time::Instant::now() + ACCEPTANCE_EVALUATION_PERSIST_ACK_RECHECK;
                if let Some(result) = waiter.wait_until(recheck) {
                    return result.map_err(|error| format!("{error:?}"));
                }
                let inner = self.inner.lock().expect("state mutex poisoned");
                let Some(index) = acceptance_evaluation_attempt_index_locked(&inner, authority)
                else {
                    return Err("the delegation is gone".to_owned());
                };
                if inner.delegations[index] != expected {
                    break inner.delegations[index].clone();
                }
            };
            waiter.abandon();
            if current
                .acceptance_evaluation
                .as_ref()
                .map(|target| &target.submission)
                != written_submission
            {
                return Err("the submission state changed before it was acknowledged".to_owned());
            }
            if attempts == ACCEPTANCE_EVALUATION_PERSIST_ACK_ATTEMPTS {
                return Err(format!(
                    "the delegation kept changing through {attempts} acknowledgement attempts"
                ));
            }
            attempts += 1;
            expected = current;
        }
    }

    /// Back to `none`, which only the request that entered from `none` may do,
    /// and only after a first send that positively recorded nothing (or before
    /// any send). It moves nothing but this request's own `pending`: never an
    /// `unconfirmed`, never another digest. `none` is the truth whether or not
    /// it reaches disk (disk then still says `pending`, the safe side), so a
    /// failed persist is logged and the evaluator's answer is unchanged.
    fn withdraw_own_acceptance_evaluation_pending(
        &self,
        authority: &AcceptanceEvaluationSubmitAuthority,
        own_payload_digest: &str,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = acceptance_evaluation_attempt_index_locked(&inner, authority) else {
            return;
        };
        let Some(target) = inner.delegations[index].acceptance_evaluation.as_mut() else {
            return;
        };
        if !matches!(
            &target.submission,
            AcceptanceEvaluationSubmission::Pending { payload_digest, .. }
                if payload_digest == own_payload_digest
        ) {
            return;
        }
        target.submission = AcceptanceEvaluationSubmission::None;
        inner.mark_delegation_mutated(index);
        if let Err(err) = self.commit_locked(&mut inner) {
            eprintln!(
                "acceptance evaluation> failed to persist the withdrawn submission of `{}`: {err:#}",
                authority.delegation_id
            );
        }
    }

    /// Keeps the write open as `unconfirmed`, carrying what it was sent as and
    /// the latest detail first in its bounded reason. If that cannot be made
    /// durable memory keeps it and the failure is logged: disk still holds the
    /// acknowledged `pending`, which reads the same.
    fn keep_acceptance_evaluation_unconfirmed(
        &self,
        authority: &AcceptanceEvaluationSubmitAuthority,
        payload_digest: String,
        original: AcceptanceEvaluationOpenWrite,
        earlier: Option<&str>,
        detail: &str,
    ) -> String {
        let reason = truncate_chars(
            &match earlier {
                Some(earlier) if earlier != detail => format!("{detail}; earlier: {earlier}"),
                _ => detail.to_owned(),
            },
            1_000,
        );
        let unconfirmed = AcceptanceEvaluationSubmission::Unconfirmed {
            payload_digest,
            reason: reason.clone(),
            at: stamp_now(),
            original,
        };
        if let Err(err) = self.settle_open_acceptance_evaluation_submission(authority, unconfirmed)
        {
            eprintln!(
                "acceptance evaluation> failed to persist the unconfirmed submission of `{}`: {err}",
                authority.delegation_id
            );
        }
        reason
    }

    /// Moves an open write forwards, to `unconfirmed` or `recorded`, wherever
    /// this exact attempt still exists, and returns once that is acknowledged
    /// durable. Never backwards: `recorded` is final, and `none` is not
    /// reachable from here. `recorded` that is not acknowledged goes back to
    /// the open state it replaced: memory never says recorded while disk may
    /// not, and the identical resend recovers it.
    fn settle_open_acceptance_evaluation_submission(
        &self,
        authority: &AcceptanceEvaluationSubmitAuthority,
        next: AcceptanceEvaluationSubmission,
    ) -> std::result::Result<(), String> {
        let recorded = matches!(next, AcceptanceEvaluationSubmission::Recorded { .. });
        let (written, previous, dispatch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = acceptance_evaluation_attempt_index_locked(&inner, authority) else {
                return Ok(());
            };
            let Some(target) = inner.delegations[index].acceptance_evaluation.as_mut() else {
                return Ok(());
            };
            if matches!(
                target.submission,
                AcceptanceEvaluationSubmission::Recorded { .. }
            ) || matches!(next, AcceptanceEvaluationSubmission::None)
            {
                return Ok(());
            }
            let previous = std::mem::replace(&mut target.submission, next);
            inner.mark_delegation_mutated(index);
            match self.commit_locked_with_persist_dispatch(&mut inner) {
                Ok((_, dispatch)) => (inner.delegations[index].clone(), previous, dispatch),
                Err(err) => {
                    if recorded {
                        if let Some(target) = inner.delegations[index].acceptance_evaluation.as_mut()
                        {
                            target.submission = previous;
                        }
                    }
                    return Err(format!("{err:#}"));
                }
            }
        };
        if dispatch == PersistDispatch::Synchronous {
            return Ok(());
        }
        let confirmed = self.confirm_acceptance_evaluation_submission_durable(authority, &written);
        if confirmed.is_err() && recorded {
            self.restore_unacknowledged_acceptance_evaluation_recorded(authority, &written, previous);
        }
        confirmed
    }

    /// Puts the open state back where the record still carries exactly the
    /// `recorded` this request wrote, and queues that for the writer.
    fn restore_unacknowledged_acceptance_evaluation_recorded(
        &self,
        authority: &AcceptanceEvaluationSubmitAuthority,
        written: &DelegationRecord,
        open: AcceptanceEvaluationSubmission,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = acceptance_evaluation_attempt_index_locked(&inner, authority) else {
            return;
        };
        let written = written
            .acceptance_evaluation
            .as_ref()
            .map(|target| &target.submission);
        let Some(target) = inner.delegations[index].acceptance_evaluation.as_mut() else {
            return;
        };
        if Some(&target.submission) != written {
            return;
        }
        target.submission = open;
        inner.mark_delegation_mutated(index);
        if let Err(err) = self.commit_locked(&mut inner) {
            eprintln!(
                "acceptance evaluation> failed to persist the restored submission state of `{}`: {err:#}",
                authority.delegation_id
            );
        }
    }
}

/// The delegation that still carries the attempt an admitted submission ran
/// against.
fn acceptance_evaluation_attempt_index_locked(
    inner: &StateInner,
    authority: &AcceptanceEvaluationSubmitAuthority,
) -> Option<usize> {
    inner
        .find_delegation_index(&authority.delegation_id)
        .filter(|index| {
            inner.delegations[*index]
                .acceptance_evaluation
                .as_ref()
                .is_some_and(|current| current.attempt_key == authority.target.attempt_key)
        })
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
