// Criterion evidence discovery and selection for acceptance briefs.
// Owns explicit requester associations, exact older-record reads and the
// canonical bound-criterion closure index. Does not judge freshness or grant
// citation authority; Engram still decides whether a submitted record counts.

const MAX_ACCEPTANCE_SELECTED_RECORDS: usize = 16;
const MAX_ACCEPTANCE_BINDING_EVIDENCE_PAGES: usize = 16;
const ACCEPTANCE_CRITERION_EVIDENCE_READ_BUDGET: Duration = Duration::from_secs(60);
// Reserve a share of the same deadline for the closing show/core bracket.
// Each closing call funds its lock retry from the remaining share.
const ACCEPTANCE_EVIDENCE_CLOSING_RESERVE: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptanceEvidenceIdentity {
    work_id: String,
    work_ref: String,
    revision: i64,
    active_run_id: Option<String>,
    root_execution_id: Option<String>,
    run_generation: Option<i64>,
}

struct AcceptanceEvidenceDiscovery {
    target: Option<EngramBindingTarget>,
    identity: Option<AcceptanceEvidenceIdentity>,
    unavailable: &'static str,
}

fn acceptance_core_identity(value: &Value) -> Result<Option<AcceptanceEvidenceIdentity>, ApiError> {
    let work = &value["status"]["work"];
    if !work.is_object()
        || !work["short_ref"]
            .as_str()
            .is_some_and(is_acceptance_evaluation_work_ref)
    {
        return Err(ApiError::bad_gateway(
            "engram work core inspect: malformed work/run identity",
        ));
    }
    // Old projections genuinely lacking both identities cannot supply an index.
    if work.get("work_id").is_none() && work.get("active_run_id").is_none() {
        return Ok(None);
    }
    let invalid = || ApiError::bad_gateway("engram work core inspect: malformed work/run identity");
    let id = |value: &Value| -> Result<String, ApiError> {
        let text = value.as_str().ok_or_else(invalid)?;
        uuid::Uuid::parse_str(text).map_err(|_| invalid())?;
        Ok(text.to_owned())
    };
    let work_id = id(&work["work_id"])?;
    let work_ref = work["short_ref"]
        .as_str()
        .filter(|text| is_acceptance_evaluation_work_ref(text))
        .ok_or_else(invalid)?
        .to_owned();
    let revision = work["revision"]
        .as_i64()
        .filter(|revision| *revision > 0)
        .ok_or_else(invalid)?;
    let active_run = work.get("active_run_id").ok_or_else(invalid)?;
    let active_run_id = if active_run.is_null() {
        None
    } else {
        Some(id(active_run)?)
    };
    let (root_execution_id, run_generation) = if let Some(run_id) = &active_run_id {
        let run = &value["run"];
        if id(&run["work_id"])? != work_id || id(&run["run_id"])? != *run_id {
            return Err(invalid());
        }
        let root = run.get("root_execution_id").map(id).transpose()?;
        let generation = run
            .get("generation")
            .map(|value| {
                value
                    .as_i64()
                    .filter(|generation| *generation > 0)
                    .ok_or_else(invalid)
            })
            .transpose()?;
        (root, generation)
    } else {
        // Core can include its latest historical run when none is active.
        // Validate that association without adopting it as current authority.
        if let Some(run) = value.get("run").filter(|run| !run.is_null()) {
            if id(&run["work_id"])? != work_id {
                return Err(invalid());
            }
            id(&run["run_id"])?;
        }
        (None, None)
    };
    Ok(Some(AcceptanceEvidenceIdentity {
        work_id,
        work_ref,
        revision,
        active_run_id,
        root_execution_id,
        run_generation,
    }))
}

fn validate_acceptance_projection_identity(
    work: &Value,
    identity: &AcceptanceEvidenceIdentity,
) -> Result<(), ApiError> {
    if work
        .get("short_ref")
        .is_some_and(|value| value.as_str() != Some(&identity.work_ref))
        || work
            .get("revision")
            .is_some_and(|value| value.as_i64() != Some(identity.revision))
        || work
            .get("work_id")
            .is_some_and(|value| value.as_str() != Some(&identity.work_id))
        || work.get("active_run_id").is_some_and(|value| match value {
            Value::Null => identity.active_run_id.is_some(),
            Value::String(run) => identity.active_run_id.as_ref() != Some(run),
            _ => true,
        })
    {
        return Err(ApiError::conflict(
            "the task's canonical work/run identity changed while evidence was read; request a new evaluation",
        ));
    }
    Ok(())
}

/// Agent receipts have operation-specific headers. Their scalar task/run
/// basis is separate from the project catalog cut used by notes cursors.
#[derive(Clone, Deserialize)]
struct AcceptanceAgentEvidenceBasis {
    acceptance_basis: i64,
    evidence_basis: i64,
}

#[derive(Deserialize)]
struct AcceptanceContractIdentity {
    short_ref: String,
    revision: i64,
}

fn acceptance_full_contract_identity(
    full: &Value,
    identity: &AcceptanceEvidenceIdentity,
) -> Result<(), ApiError> {
    let contract: AcceptanceContractIdentity = serde_json::from_value(full["work"].clone())
        .map_err(|_| {
            ApiError::bad_gateway("engram work show --full: malformed contract identity")
        })?;
    if contract.short_ref != identity.work_ref || contract.revision != identity.revision {
        return Err(ApiError::conflict(
            "engram work show --full: contract identity changed during discovery",
        ));
    }
    validate_acceptance_projection_identity(&full["work"], identity)
}

#[derive(Clone, Deserialize)]
struct AcceptanceNotesWindow {
    selection: String,
    order: String,
    newer: u64,
    older: u64,
    shown: u64,
    total: u64,
    after: Option<String>,
    includes_gates: bool,
    read_cut: AcceptanceNotesCatalogCut,
}

#[derive(Clone, Deserialize)]
struct AcceptanceNotesCatalogCut {
    project_position: i64,
    observed_at: String,
    valid_until_ms: Option<i64>,
}

fn acceptance_initial_evidence_basis(
    show: &Value,
    identity: &AcceptanceEvidenceIdentity,
) -> Result<AcceptanceAgentEvidenceBasis, ApiError> {
    let work = &show["status"]["work"];
    if work["short_ref"].as_str() != Some(identity.work_ref.as_str()) {
        return Err(ApiError::conflict(
            "engram work show: initial carrier has a missing or inconsistent work reference",
        ));
    }
    validate_acceptance_projection_identity(work, identity)?;
    let basis: AcceptanceAgentEvidenceBasis = serde_json::from_value(show.clone())
        .map_err(|_| ApiError::bad_gateway("engram work show: malformed initial evidence basis"))?;
    if basis.acceptance_basis != identity.revision || basis.evidence_basis < 0 {
        return Err(ApiError::conflict(
            "the task revision changed after the opening core identity; request a new evaluation",
        ));
    }
    Ok(basis)
}

fn acceptance_notes_window(page: &Value) -> Result<AcceptanceNotesWindow, ApiError> {
    let invalid =
        || ApiError::bad_gateway("engram work show notes continuation: malformed catalog window");
    let window: AcceptanceNotesWindow =
        serde_json::from_value(page["notes_window"].clone()).map_err(|_| invalid())?;
    let notes = page["notes"].as_array().ok_or_else(invalid)?;
    if window.selection != "newest_first"
        || window.order != "oldest_first"
        || !window.includes_gates
        || window.shown != notes.len() as u64
        || window
            .newer
            .checked_add(window.shown)
            .and_then(|n| n.checked_add(window.older))
            != Some(window.total)
        || window.read_cut.project_position < 0
        || window.read_cut.observed_at.is_empty()
        || window
            .read_cut
            .valid_until_ms
            .is_some_and(|until| until <= 0)
        || !page["notes_window"]
            .as_object()
            .is_some_and(|object| object.contains_key("after"))
        || window
            .after
            .as_ref()
            .is_some_and(|token| token.is_empty() || token.len() > 8192)
        || (window.older == 0) != window.after.is_none()
    {
        return Err(invalid());
    }
    Ok(window)
}

fn acceptance_compact_notes_carrier(
    page: &Value,
    identity: &AcceptanceEvidenceIdentity,
    basis: &AcceptanceAgentEvidenceBasis,
    previous: &AcceptanceNotesWindow,
    seen: &BTreeSet<String>,
) -> Result<AcceptanceNotesWindow, ApiError> {
    let malformed =
        || ApiError::bad_gateway("engram work show notes continuation: malformed compact header");
    let work = page
        .get("work")
        .filter(|work| work.is_object())
        .ok_or_else(malformed)?;
    work.get("short_ref")
        .and_then(Value::as_str)
        .filter(|reference| is_acceptance_evaluation_work_ref(reference))
        .ok_or_else(malformed)?;
    validate_acceptance_projection_identity(work, identity).map_err(|_| {
        ApiError::conflict("engram work show notes continuation: canonical work identity changed")
    })?;
    // A variant may expose extra assertions. Absence is allowed, but an
    // explicit null, wrong type or contradictory duplicate is not absence.
    if let Some(status) = page.get("status") {
        let duplicate = status
            .get("work")
            .filter(|work| work.is_object())
            .ok_or_else(malformed)?;
        validate_acceptance_projection_identity(duplicate, identity).map_err(|_| {
            ApiError::conflict(
                "engram work show notes continuation: contradictory duplicate identity",
            )
        })?;
    }
    for (field, expected) in [
        ("acceptance_basis", basis.acceptance_basis),
        ("evidence_basis", basis.evidence_basis),
    ] {
        if page
            .get(field)
            .is_some_and(|value| value.as_i64() != Some(expected))
        {
            return Err(ApiError::conflict(
                "engram work show notes continuation: task or evidence basis changed",
            ));
        }
    }
    let window = acceptance_notes_window(page)?;
    if window.total != previous.total
        || window.read_cut.project_position != previous.read_cut.project_position
        || previous.newer.checked_add(previous.shown) != Some(window.newer)
        || window.shown == 0
        || window
            .after
            .as_ref()
            .is_some_and(|token| seen.contains(token))
    {
        return Err(ApiError::conflict(
            "engram work show notes continuation: catalog cut, counts or cursor changed",
        ));
    }
    Ok(window)
}

fn acceptance_criterion_evidence_read_timeout(
    deadline: std::time::Instant,
    now: &impl Fn() -> std::time::Instant,
) -> Result<Duration, ApiError> {
    // The CLI runner may retry once after a lock delay. Fund both attempts
    // inside the discovery deadline instead of extending the HTTP request.
    let remaining = deadline.saturating_duration_since(now());
    let timeout = remaining.saturating_sub(ENGRAM_WORK_BINDING_LOCK_RETRY_DELAY) / 2;
    if timeout.is_zero() {
        return Err(ApiError::conflict(
            "criterion evidence read budget was spent; no evaluation was started",
        ));
    }
    Ok(timeout.min(ENGRAM_WORK_BINDING_COMMAND_TIMEOUT))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceCriterionEvidenceRequest {
    criterion: usize,
    locators: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceCriterionEvidence {
    criterion: usize,
    locators: Vec<String>,
    association: String,
}

fn acceptance_canonical_index_unavailable(task: &mut AcceptanceEvaluationTask, reason: &str) {
    for binding in &task.bindings {
        task.criterion_evidence.push(AcceptanceCriterionEvidence {
            criterion: binding.criterion,
            locators: Vec::new(),
            association: format!(
                "canonical index unavailable: {reason}; read proof through existing tracker tools"
            ),
        });
    }
    task.criterion_evidence.sort_by_key(|row| row.criterion);
}

fn acceptance_binding_read_unsupported(error: &EngramTransportError) -> bool {
    error.kind == EngramTransportErrorKind::Remote
        && (matches!(
            error.code.as_deref(),
            Some("unsupported_operation" | "unknown_operation")
        ) || (error.code.as_deref() == Some("invalid_request")
            && error
                .message
                .contains("unknown variant `acceptance_binding_read`")))
}

fn acceptance_core_inspect_unsupported(error: &EngramTransportError) -> bool {
    (error.kind == EngramTransportErrorKind::Remote
        && matches!(
            error.code.as_deref(),
            Some("unsupported_operation" | "unknown_operation")
        ))
        || (error.kind == EngramTransportErrorKind::Transport
            && (error.message.contains("unrecognized subcommand 'inspect'")
                || error.message.contains("unrecognized subcommand 'core'")))
}

fn acceptance_binding_read_error(error: EngramTransportError) -> ApiError {
    if error.kind == EngramTransportErrorKind::Remote
        && matches!(
            error.code.as_deref(),
            Some(
                "acceptance_binding_read_wrong_revision"
                    | "acceptance_binding_read_wrong_run"
                    | "acceptance_binding_read_stale_cut"
                    | "control_session_not_bound"
                    | "control_session_token_mismatch"
                    | "control_connection_superseded"
                    | "invalid_control_session"
            )
        )
    {
        ApiError::conflict(format!(
            "canonical criterion evidence basis or authority changed ({}); request a new evaluation",
            error.code.as_deref().unwrap_or("unknown")
        ))
    } else {
        acceptance_evaluation_transport_error("acceptance_binding_read", error)
    }
}

fn validate_acceptance_criterion_evidence_request(
    links: &[AcceptanceCriterionEvidenceRequest],
) -> Result<(), ApiError> {
    let mut criteria = BTreeSet::new();
    let mut records = BTreeSet::new();
    for link in links {
        if link.criterion == 0
            || !criteria.insert(link.criterion)
            || link.locators.is_empty()
            || link.locators.len() > MAX_ACCEPTANCE_SELECTED_RECORDS
            || link.locators.iter().collect::<BTreeSet<_>>().len() != link.locators.len()
        {
            return Err(ApiError::bad_request(
                "criterionEvidence needs distinct positive criterion positions and at least one locator per criterion",
            ));
        }
        for locator in &link.locators {
            if !is_acceptance_record_id(locator) {
                return Err(ApiError::bad_request(
                    "criterionEvidence locators must be full 32- or 64-character lowercase hexadecimal record ids; prefixes and inherited member locators are unsupported",
                ));
            }
            records.insert(locator);
        }
    }
    if links.len() > MAX_ACCEPTANCE_SELECTED_RECORDS
        || records.len() > MAX_ACCEPTANCE_SELECTED_RECORDS
    {
        return Err(ApiError::bad_request(
            "criterionEvidence may select at most 16 criteria and 16 distinct records",
        ));
    }
    Ok(())
}

fn read_requested_acceptance_evidence(
    task: &mut AcceptanceEvaluationTask,
    links: &[AcceptanceCriterionEvidenceRequest],
    connection: &EngramConnectionConfig,
    show_args: &[String],
    read: &impl Fn(&EngramConnectionConfig, &[String], Duration) -> Result<Value, EngramTransportError>,
    deadline: std::time::Instant,
    now: &impl Fn() -> std::time::Instant,
) -> Result<(), ApiError> {
    if links.is_empty() {
        return Ok(());
    }
    if links
        .iter()
        .any(|link| link.criterion > task.criteria.len())
    {
        return Err(ApiError::bad_request(
            "criterionEvidence names a criterion not present on this task",
        ));
    }
    let mut fetched = BTreeSet::new();
    for link in links {
        for locator in &link.locators {
            if !fetched.insert(locator.clone()) {
                continue;
            }
            // Exact reads replace even a visible summary: the brief needs the
            // stored record, not whichever fragment the newest window held.
            let mut args = show_args.to_vec();
            args.truncate(
                args.iter()
                    .position(|arg| arg == "--notes")
                    .expect("show args contain notes"),
            );
            args.extend(["--note".to_owned(), locator.clone(), "--json".to_owned()]);
            let result = read(
                connection,
                &args,
                acceptance_criterion_evidence_read_timeout(deadline, now)?,
            )
            .map_err(|error| {
                acceptance_evaluation_transport_error("engram work show --note", error)
            })?;
            if result.get("work_ref").and_then(Value::as_str) != Some(task.work_ref.as_str()) {
                return Err(ApiError::bad_gateway(
                    "selected evidence receipt belongs to another task",
                ));
            }
            let note: EngramShowNoteForEvaluation = serde_json::from_value(result["note"].clone())
                .map_err(|error| {
                    ApiError::bad_gateway(format!("selected evidence: invalid record: {error}"))
                })?;
            if note.locator != *locator
                || note.non_holder
                || note.body_omitted
                || note.summary_truncated
            {
                return Err(ApiError::conflict(
                    "selected evidence was not returned whole as a citable record of this task",
                ));
            }
            let summary = note
                .summary
                .ok_or_else(|| ApiError::conflict("selected evidence has no readable body"))?;
            if summary.len() > 16 * 1024 {
                return Err(ApiError::conflict(
                    "selected evidence body exceeds the brief read bound; supply a smaller proof record",
                ));
            }
            let is_verification = note.kind.as_str() == Some("verification");
            let evidence = AcceptanceEvaluationEvidence {
                locator: note.locator,
                kind: if is_verification {
                    "verification"
                } else {
                    match note.family.as_str() {
                        Some("notes") => "note",
                        Some("gates") => "gate",
                        Some("observations") => "observation",
                        _ => "record",
                    }
                }
                .to_owned(),
                by: note.by,
                created_at: note.created_at,
                body_bytes: note.body_bytes,
                summary: Some(summary),
                non_holder: false,
                cut_by_tracker: false,
                verification: is_verification
                    .then(|| {
                        note.verification
                            .as_ref()
                            .and_then(acceptance_evidence_verification)
                    })
                    .flatten(),
            };
            if let Some(window_entry) = task
                .evidence
                .iter_mut()
                .find(|entry| entry.locator == *locator)
            {
                *window_entry = evidence.clone();
            }
            task.indexed_evidence.push(evidence);
        }
        task.criterion_evidence.push(AcceptanceCriterionEvidence {
            criterion: link.criterion,
            locators: link.locators.clone(),
            association: "requester".to_owned(),
        });
    }
    // A targeted record read is not a snapshot of the run. Revalidate the
    // task's bases after all reads; never mix proof across a moving item.
    let current = read(
        connection,
        show_args,
        acceptance_criterion_evidence_read_timeout(deadline, now)?,
    )
    .map_err(|error| {
        acceptance_evaluation_transport_error("engram work show evidence revalidation", error)
    })?;
    validate_acceptance_evidence_basis(task, &current)
}

fn validate_acceptance_evidence_basis(
    task: &AcceptanceEvaluationTask,
    current: &Value,
) -> Result<(), ApiError> {
    if let Some(identity) = &task.canonical_identity {
        validate_acceptance_projection_identity(&current["status"]["work"], identity)?;
    }
    if current["acceptance_basis"].as_i64() != Some(task.acceptance_basis)
        || current["evidence_basis"].as_i64() != Some(task.evidence_basis)
        || current
            .pointer("/status/work/short_ref")
            .and_then(Value::as_str)
            != Some(task.work_ref.as_str())
        || current
            .pointer("/status/work/active_run_id")
            .is_some_and(|value| {
                value.as_str()
                    != task
                        .canonical_identity
                        .as_ref()
                        .and_then(|identity| identity.active_run_id.as_deref())
                        .or(task.active_run_id.as_deref())
            })
        || current
            .pointer("/status/work/work_id")
            .is_some_and(|value| {
                value.as_str()
                    != task
                        .canonical_identity
                        .as_ref()
                        .map(|identity| identity.work_id.as_str())
                        .or(task.work_id.as_deref())
            })
    {
        return Err(ApiError::conflict(
            "the task or its evidence changed while criterion evidence was read; request a new evaluation",
        ));
    }
    Ok(())
}

fn render_acceptance_criterion_evidence(
    task: &AcceptanceEvaluationTask,
    detail: AcceptanceOmissionDetail,
    fully_rendered: &BTreeSet<String>,
    same_session: bool,
) -> String {
    if task.criterion_evidence.is_empty() {
        return String::new();
    }
    if detail == AcceptanceOmissionDetail::Minimal {
        let unavailable = task
            .criterion_evidence
            .iter()
            .any(|row| row.association.starts_with("canonical index unavailable"));
        return format!(
            "Criterion evidence details not shown to fit the complete criteria. {}{}\n",
            if same_session {
                "Read the evidence with your own permitted show tools before judging it."
            } else {
                "Associations and bodies omitted from this brief were not given to you; where a verdict depends on them, give insufficient-evidence."
            },
            if unavailable {
                " The canonical index was unavailable; no original closure was inferred."
            } else {
                ""
            }
        );
    }
    let mut lines = vec![
        "Criterion evidence index (associations guide discovery; they do not establish a pass):"
            .to_owned(),
    ];
    let mut shown = BTreeSet::new();
    for criterion in 1..=task.criteria.len() {
        let rows: Vec<_> = task
            .criterion_evidence
            .iter()
            .filter(|row| row.criterion == criterion)
            .collect();
        if rows.is_empty() {
            lines.push(format!("  Criterion {criterion}: no explicit or canonical association supplied; this does not mean evidence is absent."));
        }
        for row in rows {
            lines.push(format!(
                "  Criterion {criterion}: {} ({})",
                row.locators.join(", "),
                row.association
            ));
            for locator in &row.locators {
                if !shown.insert(locator) || fully_rendered.contains(locator) {
                    continue;
                }
                if let Some(evidence) = task
                    .indexed_evidence
                    .iter()
                    .chain(task.evidence.iter())
                    .find(|entry| &entry.locator == locator)
                {
                    lines.push(
                        acceptance_brief_evidence_line(
                            evidence,
                            detail == AcceptanceOmissionDetail::Compact,
                        )
                        .0,
                    );
                }
            }
        }
    }
    lines.push("An original closure records what satisfied an obligation then; it does not prove that the check is current. Verify its source, command and result before a verdict. Captured notes-window omission counts may include records read separately in this index.\n".to_owned());
    lines.join("\n")
}

fn acceptance_criterion_evidence_cuts(
    task: &AcceptanceEvaluationTask,
    cuts: &mut AcceptanceBriefCuts,
    detail: AcceptanceOmissionDetail,
    fully_rendered: &BTreeSet<String>,
) {
    for evidence in &task.indexed_evidence {
        if !fully_rendered.contains(&evidence.locator)
            && !cuts.left_out.contains(&evidence.locator)
            && !cuts.clipped.contains(&evidence.locator)
            && !cuts.cut_by_tracker.contains(&evidence.locator)
        {
            cuts.left_out.push(evidence.locator.clone());
        }
    }
    if detail == AcceptanceOmissionDetail::Minimal {
        return;
    }
    for locator in task
        .criterion_evidence
        .iter()
        .flat_map(|row| &row.locators)
        .collect::<BTreeSet<_>>()
    {
        if fully_rendered.contains(locator) {
            continue;
        }
        let Some(evidence) = task
            .indexed_evidence
            .iter()
            .chain(task.evidence.iter())
            .find(|entry| &entry.locator == locator)
        else {
            continue;
        };
        let clipped =
            acceptance_brief_evidence_line(evidence, detail == AcceptanceOmissionDetail::Compact).1;
        cuts.left_out.retain(|entry| entry != locator);
        if !clipped {
            cuts.clipped.retain(|entry| entry != locator);
        } else if !cuts.clipped.contains(locator) {
            cuts.clipped.push(locator.clone());
        }
        if evidence.cut_by_tracker && !cuts.cut_by_tracker.contains(locator) {
            cuts.cut_by_tracker.push(locator.clone());
        } else if !evidence.cut_by_tracker && !clipped {
            cuts.cut_by_tracker.retain(|entry| entry != locator);
        }
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AcceptanceBindingEvidenceBasis {
    project_id: String,
    work_id: String,
    work_revision: i64,
    run_id: String,
    run_cut: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceBindingEvidencePage {
    basis: AcceptanceBindingEvidenceBasis,
    total: usize,
    earlier: usize,
    shown: usize,
    omitted: usize,
    rows: Vec<AcceptanceBindingEvidenceRow>,
    continuation: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceBindingEvidenceRow {
    criterion: usize,
    binding: Option<AcceptanceBindingEvidenceBinding>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceBindingEvidenceBinding {
    requirement: EngramAcceptanceRequirementForEvaluation,
    obligation: Option<AcceptanceBindingEvidenceObligation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceBindingEvidenceObligation {
    obligation_id: String,
    definition: String,
    work_revision: i64,
    rule: Value,
    triggering_observation: String,
    trigger_position: i64,
    definition_position: i64,
    state: String,
    resolution: Option<AcceptanceBindingEvidenceResolution>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceBindingEvidenceResolution {
    record: String,
    position: i64,
    kind: String,
    satisfaction: Option<AcceptanceBindingEvidenceSatisfaction>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceBindingEvidenceSatisfaction {
    evaluated_cut: i64,
    verification: AcceptanceBindingEvidenceVerification,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceBindingEvidenceVerification {
    record: String,
    position: i64,
    check_kind: String,
    check_fingerprint: String,
    result: String,
    source_basis: EngramExecutionSourceBasis,
    producer: AcceptanceBindingEvidenceProducer,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceBindingEvidenceProducer {
    record: String,
    position: i64,
    outcome: String,
}

impl AppState {
    fn acceptance_evidence_discovery_target(
        &self,
        session_id: &str,
        store: &EngramAuthorityStoreKey,
    ) -> Result<AcceptanceEvidenceDiscovery, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let target = Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
            .map_err(ApiError::conflict)?;
        let mut discovery = AcceptanceEvidenceDiscovery {
            target: None,
            identity: None,
            unavailable: "no existing turn-gated host authority",
        };
        let Some(target) = target else {
            return Ok(discovery);
        };
        if target.settings.authority_store_key.as_ref() != Some(store) {
            return Err(ApiError::conflict(
                "criterion binding evidence authority belongs to another tracker store",
            ));
        }
        if target.routing_token.is_none() {
            discovery.unavailable = "no current host routing token";
        } else if target.circuit_open || target.rebind_required {
            discovery.unavailable = "current control authority requires recovery";
        } else {
            discovery.target = Some(target);
            discovery.unavailable = "core receipt has no work/run identity";
        }
        Ok(discovery)
    }

    fn revalidate_acceptance_discovery_authority(
        &self,
        session_id: &str,
        store: &EngramAuthorityStoreKey,
        discovery: &AcceptanceEvidenceDiscovery,
        reader: &EngramConnectionConfig,
    ) -> Result<(), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let current_reader = acceptance_evaluation_host_target_locked(&inner, session_id)?;
        if current_reader.store != *store || current_reader.connection != *reader {
            return Err(ApiError::conflict(
                ACCEPTANCE_EVALUATION_STORE_CHANGED_ERROR,
            ));
        }
        if let Some(opening) = &discovery.target {
            let current =
                Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                    .map_err(ApiError::conflict)?;
            if current.is_none_or(|current| {
                current.settings.authority_store_key.as_ref() != Some(store)
                    || current.connection != opening.connection
                    || current.routing_token != opening.routing_token
                    || current.project_reset_owner_generation
                        != opening.project_reset_owner_generation
                    || !Arc::ptr_eq(&current.adapter, &opening.adapter)
                    || current.circuit_open
                    || current.rebind_required
            }) {
                return Err(ApiError::conflict(
                    "canonical criterion evidence authority changed during discovery; request a new evaluation",
                ));
            }
        }
        Ok(())
    }

    fn read_acceptance_binding_evidence(
        &self,
        store: &EngramAuthorityStoreKey,
        task: &mut AcceptanceEvaluationTask,
        discovery: &AcceptanceEvidenceDiscovery,
        deadline: std::time::Instant,
        now: &impl Fn() -> std::time::Instant,
    ) -> Result<(), ApiError> {
        if task.bindings.is_empty() {
            return Ok(());
        }
        let (Some(target), Some(identity)) = (&discovery.target, &discovery.identity) else {
            acceptance_canonical_index_unavailable(task, discovery.unavailable);
            return Ok(());
        };
        let Some(run_id) = &identity.active_run_id else {
            acceptance_canonical_index_unavailable(task, "work has no active run");
            return Ok(());
        };
        let token = target
            .routing_token
            .as_ref()
            .expect("discovery captured a routing token");
        let expected = AcceptanceBindingEvidenceBasis {
            project_id: store.project_id.clone(),
            work_id: identity.work_id.clone(),
            work_revision: task.acceptance_basis,
            run_id: run_id.clone(),
            run_cut: task.evidence_basis,
        };
        let mut after = None;
        let mut rows = Vec::new();
        let mut cursors = BTreeSet::new();
        // Engram's complete rows have a fixed eight-row/16-KiB page bound.
        // A task with more than 128 criteria is refused, never silently partial.
        for _ in 0..MAX_ACCEPTANCE_BINDING_EVIDENCE_PAGES {
            let request = EngramControlRequest::AcceptanceBindingRead {
                routing_token: token.clone(),
                work_id: expected.work_id.clone(),
                expected_work_revision: expected.work_revision,
                run_id: expected.run_id.clone(),
                after: after.take(),
            };
            let value = match target.adapter.request(
                &target.connection,
                &request,
                acceptance_criterion_evidence_read_timeout(deadline, now)?,
            ) {
                Ok(value) => value,
                Err(error) if rows.is_empty() && acceptance_binding_read_unsupported(&error) => {
                    acceptance_canonical_index_unavailable(
                        task,
                        "Engram does not support canonical closure discovery",
                    );
                    return Ok(());
                }
                Err(error) => return Err(acceptance_binding_read_error(error)),
            };
            if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 16 * 1024) {
                return Err(ApiError::bad_gateway(
                    "acceptance binding evidence exceeds the wire page bound",
                ));
            }
            let page: AcceptanceBindingEvidencePage =
                serde_json::from_value(value).map_err(|error| {
                    ApiError::bad_gateway(format!(
                        "acceptance binding evidence: invalid page: {error}"
                    ))
                })?;
            if page.basis != expected
                || page.total != task.criteria.len()
                || page.earlier != rows.len()
                || page.shown != page.rows.len()
                || page.shown > 8
                || page
                    .earlier
                    .checked_add(page.shown)
                    .and_then(|read| page.total.checked_sub(read))
                    != Some(page.omitted)
                || (page.omitted == 0) != page.continuation.is_none()
                || page.shown == 0 && page.omitted != 0
                || page
                    .rows
                    .iter()
                    .enumerate()
                    .any(|(offset, row)| row.criterion != rows.len() + offset + 1)
            {
                return Err(ApiError::conflict(
                    "criterion binding evidence has a different task, run, revision, cut or page boundary; request a new evaluation",
                ));
            }
            rows.extend(page.rows);
            match page.continuation {
                None => {
                    apply_acceptance_binding_evidence(task, rows)?;
                    return Ok(());
                }
                Some(cursor)
                    if !cursor.is_empty()
                        && cursor.len() <= 4096
                        && cursors.insert(cursor.clone()) =>
                {
                    after = Some(cursor)
                }
                Some(_) => {
                    return Err(ApiError::bad_gateway(
                        "criterion binding evidence returned an invalid or repeated continuation",
                    ));
                }
            }
        }
        Err(ApiError::conflict(
            "criterion binding evidence exceeded the complete-index page bound; no evaluation was started",
        ))
    }
}

fn apply_acceptance_binding_evidence(
    task: &mut AcceptanceEvaluationTask,
    rows: Vec<AcceptanceBindingEvidenceRow>,
) -> Result<(), ApiError> {
    let invalid = || {
        ApiError::bad_gateway(
            "criterion binding evidence has an invalid canonical obligation or verification",
        )
    };
    let position_valid = |position: i64| position > 0 && position <= task.evidence_basis;
    let mut additions = Vec::new();
    let mut links = Vec::new();
    for row in rows {
        let expected = task
            .bindings
            .iter()
            .find(|binding| binding.criterion == row.criterion);
        let Some(binding) = row.binding else {
            if expected.is_some() {
                return Err(invalid());
            }
            continue;
        };
        let Some(expected) = expected else {
            return Err(invalid());
        };
        if binding.requirement.check_kind != expected.check_kind
            || binding.requirement.check_fingerprint != expected.fingerprint
        {
            return Err(invalid());
        }
        let Some(obligation) = binding.obligation else {
            continue;
        };
        if obligation.obligation_id.is_empty()
            || !is_citable_acceptance_locator(&obligation.definition)
            || !is_citable_acceptance_locator(&obligation.triggering_observation)
            || obligation.work_revision > task.acceptance_basis
            || obligation.work_revision < 1
            || !position_valid(obligation.trigger_position)
            || !position_valid(obligation.definition_position)
            || obligation.trigger_position >= obligation.definition_position
            || obligation.rule.get("rule_id").and_then(Value::as_str)
                != Some(
                    format!(
                        "acceptance_criterion_requires_verification:{}",
                        row.criterion
                    )
                    .as_str(),
                )
            || obligation.rule.get("rule_version").and_then(Value::as_u64) != Some(1)
        {
            return Err(invalid());
        }
        let Some(resolution) = obligation.resolution else {
            if obligation.state != "open" {
                return Err(invalid());
            }
            continue;
        };
        if !is_citable_acceptance_locator(&resolution.record)
            || !position_valid(resolution.position)
            || obligation.definition_position >= resolution.position
            || resolution.kind != obligation.state
            || !matches!(
                resolution.kind.as_str(),
                "satisfied" | "waived" | "displaced"
            )
            || (resolution.kind == "satisfied") != resolution.satisfaction.is_some()
        {
            return Err(invalid());
        }
        let Some(satisfaction) = resolution.satisfaction else {
            continue;
        };
        let verification = satisfaction.verification;
        if !position_valid(satisfaction.evaluated_cut)
            || !position_valid(verification.position)
            || !position_valid(verification.producer.position)
            || !is_citable_acceptance_locator(&verification.record)
            || !is_citable_acceptance_locator(&verification.check_fingerprint)
            || !is_citable_acceptance_locator(&verification.producer.record)
            || verification.check_kind != expected.check_kind
            || verification.result != "passed"
            || expected
                .fingerprint
                .as_ref()
                .is_some_and(|pin| pin != &verification.check_fingerprint)
            || verification.producer.outcome != "succeeded"
            || verification.producer.position >= verification.position
            || verification.position > satisfaction.evaluated_cut
            || satisfaction.evaluated_cut >= resolution.position
            || verification.source_basis.workspace_id.is_empty()
            || verification.source_basis.workspace_id.len() > 512
            || verification.source_basis.source_revision.is_empty()
            || verification.source_basis.source_revision.len() > 96
            || !verification
                .source_basis
                .source_revision
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '.'))
        {
            return Err(invalid());
        }
        links.push(AcceptanceCriterionEvidence {
            criterion: row.criterion,
            locators: vec![verification.record.clone()],
            association: "original obligation closure".to_owned(),
        });
        additions.push(AcceptanceEvaluationEvidence {
            locator: verification.record, kind: "verification".to_owned(), by: None, created_at: None,
            summary: Some(format!("Host projection of the original recorded closure (not the stored record body) at run cut {}: command fingerprint {}; source workspace {} at {}; producer {} succeeded. This records the original closure, not current freshness.",
                satisfaction.evaluated_cut, verification.check_fingerprint, verification.source_basis.workspace_id,
                verification.source_basis.source_revision, verification.producer.record)),
            body_bytes: None, non_holder: false, cut_by_tracker: false,
            verification: Some(AcceptanceEvidenceVerification {
                check_kind: verification.check_kind, result: verification.result,
                source_revision: Some(verification.source_basis.source_revision),
            }),
        });
    }
    for evidence in additions {
        // A selected complete record remains complete. The closure association
        // adds canonical discovery without replacing its stored body.
        if !task
            .indexed_evidence
            .iter()
            .any(|entry| entry.locator == evidence.locator && !entry.cut_by_tracker)
            && !task
                .evidence
                .iter()
                .any(|entry| entry.locator == evidence.locator && !entry.cut_by_tracker)
        {
            task.indexed_evidence
                .retain(|entry| entry.locator != evidence.locator);
            task.indexed_evidence.push(evidence);
        }
    }
    task.criterion_evidence.extend(links);
    task.criterion_evidence.sort_by_key(|link| link.criterion);
    Ok(())
}
