// Acceptance-evaluation host logic that needs no state and no process: mode
// selection, the task snapshot read from `engram work show`, the evaluator
// brief, the evaluator's submission shape and CLI arguments, what one run of
// them told the host, and the bounded extract kept of a receipt. Does not own
// authority, persistence, process transport or HTTP; those live in
// acceptance_evaluation_api.rs.

const ACCEPTANCE_EVALUATION_SUBMISSION_SCHEMA_VERSION: u32 = 1;
const MAX_ACCEPTANCE_EVALUATION_WORK_REF_CHARS: usize = 128;
const MAX_ACCEPTANCE_EVALUATION_VERDICTS: usize = 256;
const MAX_ACCEPTANCE_EVALUATION_RATIONALE_CHARS: usize = 2_000;
const MAX_ACCEPTANCE_EVALUATION_EVIDENCE_PER_CRITERION: usize = 8;
const MAX_ACCEPTANCE_EVALUATION_MODEL_SEGMENT_BYTES: usize = 128;
// The brief is model input built from tracker text other sessions wrote, so
// every interpolated field is one line. Criteria are the contract the verdicts
// answer and are never cut; the rest is context and is bounded.
// One byte bound for both briefs: the evaluator's prompt must fit a delegation
// prompt, and the same-session brief is held to the same so that a contract is
// refused or briefed alike whichever mode is selected.
const MAX_ACCEPTANCE_BRIEF_BYTES: usize = MAX_DELEGATION_PROMPT_BYTES;
const MAX_ACCEPTANCE_BRIEF_TITLE_CHARS: usize = 300;
const MAX_ACCEPTANCE_BRIEF_OUTCOME_BYTES: usize = 16_000;
const ACCEPTANCE_BRIEF_OUTCOME_TRUNCATION_MARKER: &str = "[outcome truncated by the host]";
const MAX_ACCEPTANCE_BRIEF_SUMMARY_CHARS: usize = 600;
// Bounds of the stored receipt extract.
const MAX_ACCEPTANCE_RECEIPT_HASH_CHARS: usize = 128;
const MAX_ACCEPTANCE_RECEIPT_WORD_CHARS: usize = 64;
// The evaluator is handed the raw receipt once, cut to this.
const MAX_ACCEPTANCE_SUBMIT_RESPONSE_RECEIPT_BYTES: usize = 16 * 1024;
const MAX_ACCEPTANCE_BRIEF_LABEL_CHARS: usize = 120;
const MAX_ACCEPTANCE_BRIEF_EVIDENCE_ENTRIES: usize = 40;

impl AcceptanceEvaluationMode {
    fn word(self) -> &'static str {
        match self {
            Self::SameSession => "same_session",
            Self::SubAgent => "sub_agent",
            Self::IndependentSession => "independent_session",
        }
    }

    /// Engram prints underscores and accepts hyphens; accept both.
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "same_session" => Some(Self::SameSession),
            "sub_agent" => Some(Self::SubAgent),
            "independent_session" => Some(Self::IndependentSession),
            _ => None,
        }
    }
}

/// Picks who evaluates. `pin` is the task's own choice and always wins when
/// the policy admits it. `admitted` is the policy's set: `None` means the
/// host could not learn it, and the tracker, which enforces the policy when
/// the evaluation is recorded, stays the judge.
fn select_acceptance_evaluation_mode(
    pin: Option<&str>,
    admitted: Option<&[String]>,
) -> std::result::Result<AcceptanceEvaluationMode, String> {
    if admitted.is_some_and(<[String]>::is_empty) {
        return Err(
            "the project's Engram policy admits no acceptance-evaluation mode: completion is \
             self-asserted there and no evaluator is needed"
                .to_owned(),
        );
    }
    let admitted_modes = admitted.map(|words| {
        words
            .iter()
            .filter_map(|word| AcceptanceEvaluationMode::parse(word))
            .collect::<Vec<_>>()
    });
    let admitted_words = || admitted.unwrap_or_default().join(", ");
    if let Some(pin) = pin {
        let Some(mode) = AcceptanceEvaluationMode::parse(pin) else {
            return Err(format!(
                "the task pins evaluation mode `{}`, which this host does not know",
                acceptance_brief_text(pin, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS)
            ));
        };
        if admitted_modes
            .as_ref()
            .is_some_and(|modes| !modes.contains(&mode))
        {
            return Err(format!(
                "the task pins evaluation mode `{}`, but the project policy admits only: {}",
                mode.word(),
                admitted_words()
            ));
        }
        return Ok(mode);
    }
    let Some(modes) = admitted_modes else {
        return Ok(AcceptanceEvaluationMode::IndependentSession);
    };
    [
        AcceptanceEvaluationMode::IndependentSession,
        AcceptanceEvaluationMode::SubAgent,
        AcceptanceEvaluationMode::SameSession,
    ]
    .into_iter()
    .find(|mode| modes.contains(mode))
    .ok_or_else(|| {
        format!(
            "the project policy admits only evaluation modes this host does not know: {}",
            admitted_words()
        )
    })
}

/// `acceptance_evaluation.allowed_modes` from `engram control-policy show`,
/// which reads the policy head only. The `doctor` report nests the same key
/// under `control`, but audits the whole store first (over a minute on a large
/// one), so it is never the per-request read. A receipt without the key yields
/// `None`: unknown, never "none".
fn acceptance_evaluation_admitted_modes(policy: &Value) -> Option<Vec<String>> {
    policy
        .pointer("/acceptance_evaluation/allowed_modes")
        .or_else(|| policy.pointer("/control/acceptance_evaluation/allowed_modes"))
        .and_then(Value::as_array)
        .map(|modes| {
            modes
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
}

/// What the request path read, before a delegation id exists to key it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptanceEvaluationTargetSeed {
    work_ref: String,
    mode: AcceptanceEvaluationMode,
    acceptance_basis: i64,
    evidence_basis: i64,
    criteria_count: usize,
    /// The store the reads above ran against.
    store: EngramAuthorityStoreKey,
}

impl AcceptanceEvaluationTargetSeed {
    fn into_target(self, attempt_key: String) -> DelegationAcceptanceEvaluation {
        DelegationAcceptanceEvaluation {
            work_ref: self.work_ref,
            mode: self.mode,
            acceptance_basis: self.acceptance_basis,
            evidence_basis: self.evidence_basis,
            criteria_count: self.criteria_count,
            attempt_key,
            store: Some(self.store),
            submission: AcceptanceEvaluationSubmission::None,
        }
    }
}

/// A positional CLI argument: it must never read as a flag. The one rule for
/// the caller's ref and for the ref the tracker answers with, which is the one
/// that is persisted and later passed to `evaluate`.
fn is_acceptance_evaluation_work_ref(work_ref: &str) -> bool {
    !work_ref.is_empty()
        && work_ref.chars().count() <= MAX_ACCEPTANCE_EVALUATION_WORK_REF_CHARS
        && !work_ref.starts_with('-')
        && !work_ref.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn validate_acceptance_evaluation_work_ref(work_ref: &str) -> std::result::Result<(), ApiError> {
    if !is_acceptance_evaluation_work_ref(work_ref) {
        return Err(ApiError::bad_request("invalid workRef"));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct EngramShowForEvaluation {
    acceptance_basis: Option<i64>,
    evidence_basis: Option<i64>,
    status: EngramShowStatusForEvaluation,
    #[serde(default)]
    notes: Vec<EngramShowNoteForEvaluation>,
    #[serde(default)]
    notes_omitted: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct EngramShowStatusForEvaluation {
    work: EngramShowWorkForEvaluation,
}

#[derive(Debug, Deserialize)]
struct EngramShowWorkForEvaluation {
    short_ref: String,
    lifecycle: String,
    /// Present only when the task pins a mode.
    #[serde(default)]
    evaluation_mode: Option<String>,
}

/// `engram work show REF --full --json`: the complete contract. The windowed
/// read clips long titles, outcomes and criteria, and the CLI refuses `--full`
/// together with `--notes`/`--gates`, so the two facts take two reads.
#[derive(Debug, Deserialize)]
struct EngramShowFullForEvaluation {
    work: EngramShowFullWorkForEvaluation,
}

#[derive(Debug, Deserialize)]
struct EngramShowFullWorkForEvaluation {
    revision: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    outcome: String,
    #[serde(default)]
    acceptance: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EngramShowNoteForEvaluation {
    locator: String,
    /// `notes`, `gates` or `observations`: what the record is. `kind` is the
    /// storage kind and reads `generic` for all of them.
    #[serde(default)]
    family: Value,
    #[serde(default)]
    kind: Value,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    non_holder: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptanceEvaluationEvidence {
    locator: String,
    kind: String,
    by: Option<String>,
    created_at: Option<String>,
    summary: Option<String>,
    /// A non-holder observation is context; the tracker refuses it as a citation.
    non_holder: bool,
}

/// One open task as the evaluator needs it: bases, pin and evidence index from
/// `engram work show REF --notes --gates --json`, the complete title, outcome
/// and criteria from `engram work show REF --full --json`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptanceEvaluationTask {
    work_ref: String,
    title: String,
    outcome: String,
    criteria: Vec<String>,
    pinned_mode: Option<String>,
    acceptance_basis: i64,
    evidence_basis: i64,
    /// Oldest first, as the tracker's window orders them.
    evidence: Vec<AcceptanceEvaluationEvidence>,
    evidence_omitted: usize,
}

fn parse_acceptance_evaluation_task(
    windowed: Value,
    full: Value,
) -> std::result::Result<AcceptanceEvaluationTask, ApiError> {
    let show: EngramShowForEvaluation = serde_json::from_value(windowed)
        .map_err(|e| ApiError::bad_gateway(format!("engram work show: invalid receipt: {e}")))?;
    let contract: EngramShowFullForEvaluation = serde_json::from_value(full).map_err(|e| {
        ApiError::bad_gateway(format!("engram work show --full: invalid receipt: {e}"))
    })?;
    let work = show.status.work;
    // This ref, not the caller's, is persisted and becomes `evaluate`'s
    // positional argument.
    if !is_acceptance_evaluation_work_ref(&work.short_ref) {
        return Err(ApiError::bad_gateway(
            "engram work show: the receipt names a work ref that cannot be passed back to the tracker",
        ));
    }
    let work_ref = acceptance_brief_text(&work.short_ref, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS);
    if work.lifecycle != "open" {
        return Err(ApiError::conflict(format!(
            "`{work_ref}` is {}; only an open task can be evaluated",
            acceptance_brief_text(&work.lifecycle, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS)
        )));
    }
    if contract.work.acceptance.is_empty() {
        return Err(ApiError::conflict(format!(
            "`{work_ref}` has no acceptance criteria to evaluate"
        )));
    }
    let (Some(acceptance_basis), Some(evidence_basis)) =
        (show.acceptance_basis, show.evidence_basis)
    else {
        return Err(ApiError::conflict(format!(
            "`{work_ref}` reports no acceptance and evidence basis: the project's Engram policy \
             does not evaluate acceptance, or its build predates acceptance evaluation"
        )));
    };
    // The basis names the revision whose criteria the verdicts address, so the
    // criteria must come from that same revision.
    if contract.work.revision != acceptance_basis {
        return Err(ApiError::conflict(format!(
            "`{work_ref}` was revised while it was being read; request the evaluation again"
        )));
    }
    Ok(AcceptanceEvaluationTask {
        work_ref: work.short_ref,
        title: contract.work.title,
        outcome: contract.work.outcome,
        criteria: contract.work.acceptance,
        pinned_mode: work.evaluation_mode,
        acceptance_basis,
        evidence_basis,
        evidence: show
            .notes
            .into_iter()
            .map(|note| AcceptanceEvaluationEvidence {
                locator: note.locator,
                kind: match note.family.as_str() {
                    Some("gates") => "gate".to_owned(),
                    Some("notes") => "note".to_owned(),
                    Some("observations") => "observation".to_owned(),
                    _ => note.kind.as_str().unwrap_or("record").to_owned(),
                },
                by: note.by,
                created_at: note.created_at,
                summary: note.summary,
                non_holder: note.non_holder,
            })
            .collect(),
        evidence_omitted: show.notes_omitted.unwrap_or(0),
    })
}

impl AcceptanceEvaluationTask {
    fn target_seed(
        &self,
        mode: AcceptanceEvaluationMode,
        store: EngramAuthorityStoreKey,
    ) -> AcceptanceEvaluationTargetSeed {
        AcceptanceEvaluationTargetSeed {
            work_ref: self.work_ref.clone(),
            mode,
            acceptance_basis: self.acceptance_basis,
            evidence_basis: self.evidence_basis,
            criteria_count: self.criteria.len(),
            store,
        }
    }
}

/// One line: control characters (newlines included) become spaces, so tracker
/// text can never add lines or sections to a host-built brief.
fn acceptance_brief_line(value: &str) -> String {
    let flattened = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    flattened.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One bounded line.
fn acceptance_brief_text(value: &str, max_chars: usize) -> String {
    truncate_chars(&acceptance_brief_line(value), max_chars)
}

/// The outcome is context, not the contract: bounded, and the cut is said.
/// The bound is in UTF-8 bytes, the unit of the prompt cap it competes for:
/// at most `max_bytes` of the text are kept, the marker comes on top.
fn acceptance_brief_outcome(value: &str, max_bytes: usize) -> String {
    let line = acceptance_brief_line(value);
    // Never replace an outcome by a marker that is longer than the outcome.
    if line.len() <= max_bytes.max(ACCEPTANCE_BRIEF_OUTCOME_TRUNCATION_MARKER.len()) {
        return line;
    }
    let mut end = max_bytes;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let kept = line[..end].trim_end();
    if kept.is_empty() {
        return ACCEPTANCE_BRIEF_OUTCOME_TRUNCATION_MARKER.to_owned();
    }
    format!("{kept} {ACCEPTANCE_BRIEF_OUTCOME_TRUNCATION_MARKER}")
}

fn is_citable_acceptance_locator(locator: &str) -> bool {
    (8..=64).contains(&locator.len()) && is_lowercase_hex(locator)
}

/// Every criterion, complete: a verdict covers the whole criterion, so a
/// requirement the evaluator never saw would be judged unread.
fn acceptance_brief_criteria(task: &AcceptanceEvaluationTask) -> String {
    task.criteria
        .iter()
        .enumerate()
        .map(|(index, criterion)| format!("  {}. {}", index + 1, acceptance_brief_line(criterion)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn acceptance_brief_evidence_line(evidence: &AcceptanceEvaluationEvidence) -> String {
    let mut attribution = acceptance_brief_text(&evidence.kind, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS);
    if let Some(by) = evidence.by.as_deref() {
        attribution.push_str(", by ");
        attribution.push_str(&acceptance_brief_text(by, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS));
    }
    if let Some(created_at) = evidence.created_at.as_deref() {
        attribution.push_str(", ");
        attribution.push_str(&acceptance_brief_text(
            created_at,
            MAX_ACCEPTANCE_BRIEF_LABEL_CHARS,
        ));
    }
    let summary = evidence
        .summary
        .as_deref()
        .map(|summary| acceptance_brief_text(summary, MAX_ACCEPTANCE_BRIEF_SUMMARY_CHARS))
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| "(body not shown)".to_owned());
    // Saying so here spares the evaluator a refused submission.
    let citable = if evidence.non_holder || !is_citable_acceptance_locator(&evidence.locator) {
        " [context only: the tracker refuses this as a citation]"
    } else {
        ""
    };
    format!(
        "  - {} ({attribution}){citable}: {summary}",
        acceptance_brief_text(&evidence.locator, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS)
    )
}

/// The evaluator's task text. Built only from tracker reads, never from the
/// requesting session: the session whose work is judged does not get to brief
/// its judge. To fit `max_bytes` it drops the oldest evidence first and then
/// shortens the outcome; it never drops or cuts a criterion, and refuses only
/// when the criteria do not fit with no context left to give up.
fn build_acceptance_evaluator_prompt(
    task: &AcceptanceEvaluationTask,
    cwd: &str,
    max_bytes: usize,
) -> std::result::Result<String, ApiError> {
    // Context shrinks before anything is said about the criteria: evidence,
    // oldest first, down to none with the outcome still at its own bound; only
    // then the outcome, down to its marker.
    let mut shown = task.evidence.len().min(MAX_ACCEPTANCE_BRIEF_EVIDENCE_ENTRIES);
    loop {
        let prompt = render_acceptance_evaluator_prompt(
            task,
            cwd,
            shown,
            MAX_ACCEPTANCE_BRIEF_OUTCOME_BYTES,
        );
        if prompt.len() <= max_bytes {
            return Ok(prompt);
        }
        if shown == 0 {
            break;
        }
        shown -= 1;
    }
    let floor = render_acceptance_evaluator_prompt(task, cwd, 0, 0);
    if floor.len() > max_bytes {
        // True only now: no context is left to give up.
        return Err(acceptance_contract_too_large(task, floor.len(), max_bytes));
    }
    // One byte joins the kept text to the marker the floor already carries.
    let outcome_bytes = (max_bytes - floor.len())
        .saturating_sub(1)
        .min(MAX_ACCEPTANCE_BRIEF_OUTCOME_BYTES);
    Ok(render_acceptance_evaluator_prompt(task, cwd, 0, outcome_bytes))
}

fn render_acceptance_evaluator_prompt(
    task: &AcceptanceEvaluationTask,
    cwd: &str,
    shown: usize,
    outcome_bytes: usize,
) -> String {
    let omitted = task.evidence_omitted + (task.evidence.len() - shown);
    let mut evidence = task.evidence[task.evidence.len() - shown..]
        .iter()
        .map(acceptance_brief_evidence_line)
        .collect::<Vec<_>>();
    if evidence.is_empty() {
        evidence.push("  (none shown)".to_owned());
    }
    if omitted > 0 {
        evidence.push(format!("  ({omitted} older entries not shown)"));
    }
    format!(
        "You are an acceptance evaluator. Another session did the work below and asks\n\
whether it meets its acceptance criteria. You judge; you do not fix.\n\
\n\
Task {work_ref}: {title}\n\
Outcome: {outcome}\n\
\n\
Acceptance criteria — judge every one, by number:\n\
{criteria}\n\
\n\
Evidence recorded on the task — cite by locator:\n\
{evidence}\n\
\n\
The workspace at {cwd} is read-only. Inspect files, history and diffs as\n\
needed; do not edit, build or run project scripts.\n\
\n\
Rules:\n\
- Give each criterion exactly one verdict: pass, fail, insufficient-evidence\n  \
or needs-human.\n\
- A pass must cite at least one locator from the list above that supports it,\n  \
and you must have checked the claim against the workspace wherever it can be\n  \
checked. Missing proof is insufficient-evidence, never pass.\n\
- The rationale states what you checked and what you found, in one or two\n  \
sentences on a single line.\n\
- Submit once with {submit_tool}. If the host returns a\n  \
refusal, read it, correct the submission and submit again. Call no other\n  \
tracker tool.\n\
- Finish with a short plain-text summary of the verdicts; it is the `Summary:`\n  \
of the result packet described below.",
        work_ref = acceptance_brief_text(&task.work_ref, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS),
        title = acceptance_brief_text(&task.title, MAX_ACCEPTANCE_BRIEF_TITLE_CHARS),
        outcome = acceptance_brief_outcome(&task.outcome, outcome_bytes),
        criteria = acceptance_brief_criteria(task),
        evidence = evidence.join("\n"),
        cwd = acceptance_brief_text(cwd, MAX_DELEGATION_CWD_CHARS),
        submit_tool = TERMAL_SUBMIT_ACCEPTANCE_EVALUATION_TOOL_NAME,
    )
}

/// The one refusal both briefs give when the complete criteria do not fit
/// with no context left to give up. `smallest` is the brief at that point.
fn acceptance_contract_too_large(
    task: &AcceptanceEvaluationTask,
    smallest: usize,
    max_bytes: usize,
) -> ApiError {
    ApiError::conflict(format!(
        "`{}`: the acceptance contract is too large to brief an evaluator: its {} complete criteria take {} bytes, and with the host's instructions alone the brief is {smallest} bytes against a limit of {max_bytes}",
        acceptance_brief_text(&task.work_ref, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS),
        task.criteria.len(),
        acceptance_brief_criteria(task).len(),
    ))
}

/// `same_session` spawns nothing: the caller judges its own work and records
/// it through its own tracker tool, against the bases read here. It carries no
/// context to shrink, so it holds the complete criteria within the same byte
/// bound as the evaluator's brief or is refused the same way.
fn build_same_session_acceptance_brief(
    task: &AcceptanceEvaluationTask,
    max_bytes: usize,
) -> std::result::Result<String, ApiError> {
    let brief = render_same_session_acceptance_brief(task);
    if brief.len() > max_bytes {
        return Err(acceptance_contract_too_large(task, brief.len(), max_bytes));
    }
    Ok(brief)
}

fn render_same_session_acceptance_brief(task: &AcceptanceEvaluationTask) -> String {
    format!(
        "Acceptance evaluation of {work_ref} runs in this session (mode same_session); no \
evaluator was spawned.\n\
\n\
Judge your own work against every acceptance criterion, by number:\n\
{criteria}\n\
\n\
Record it with your own Engram `evaluate` tool: mode same_session, acceptance_basis \
{acceptance_basis}, evidence_basis {evidence_basis}, and exactly one verdict per criterion \
(pass, fail, insufficient_evidence or needs_human) with a rationale saying what you checked and \
what you found. A pass must cite at least one evidence locator from `show {work_ref} --notes \
--gates`. Missing proof is insufficient_evidence, never pass.",
        work_ref = acceptance_brief_text(&task.work_ref, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS),
        criteria = acceptance_brief_criteria(task),
        acceptance_basis = task.acceptance_basis,
        evidence_basis = task.evidence_basis,
    )
}

/// Model-facing request accepted by `termal_submit_acceptance_evaluation`.
/// The work ref, mode, bases, identity, model and attempt key are the host's;
/// an evaluator supplies verdicts and nothing else.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubmitAcceptanceEvaluationRequest {
    schema_version: u32,
    verdicts: Vec<SubmitAcceptanceEvaluationVerdict>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubmitAcceptanceEvaluationVerdict {
    criterion: usize,
    verdict: String,
    #[serde(default)]
    basis: Option<String>,
    rationale: String,
    #[serde(default)]
    evidence: Vec<String>,
}

/// The tool speaks hyphenated words; the tracker's canonical words use
/// underscores. Either spelling is accepted from the evaluator.
fn acceptance_verdict_word(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "pass" => Some("pass"),
        "fail" => Some("fail"),
        "insufficient-evidence" => Some("insufficient_evidence"),
        "needs-human" => Some("needs_human"),
        _ => None,
    }
}

fn acceptance_basis_word(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "observed" => Some("observed"),
        "asserted" => Some("asserted"),
        "judgment" => Some("judgment"),
        "human-required" => Some("human_required"),
        _ => None,
    }
}

impl SubmitAcceptanceEvaluationRequest {
    /// Everything checkable without the target: also what admits the tool call
    /// for a Codex child, so a malformed call is never auto-approved.
    fn validate_shape(&self) -> std::result::Result<(), String> {
        if self.schema_version != ACCEPTANCE_EVALUATION_SUBMISSION_SCHEMA_VERSION {
            return Err(format!(
                "schemaVersion must be {ACCEPTANCE_EVALUATION_SUBMISSION_SCHEMA_VERSION}"
            ));
        }
        if self.verdicts.is_empty() {
            return Err("verdicts must name every criterion; none were given".to_owned());
        }
        if self.verdicts.len() > MAX_ACCEPTANCE_EVALUATION_VERDICTS {
            return Err(format!(
                "at most {MAX_ACCEPTANCE_EVALUATION_VERDICTS} verdicts are accepted"
            ));
        }
        let mut seen = BTreeSet::new();
        for entry in &self.verdicts {
            let position = entry.criterion;
            if position == 0 {
                return Err("criterion is the one-based number of a criterion".to_owned());
            }
            if !seen.insert(position) {
                return Err(format!("criterion {position} has more than one verdict"));
            }
            let Some(verdict) = acceptance_verdict_word(&entry.verdict) else {
                return Err(format!(
                    "criterion {position}: verdict must be pass, fail, insufficient-evidence or needs-human"
                ));
            };
            if entry
                .basis
                .as_deref()
                .is_some_and(|basis| acceptance_basis_word(basis).is_none())
            {
                return Err(format!(
                    "criterion {position}: basis must be observed, asserted, judgment or human-required"
                ));
            }
            if entry.rationale.trim().is_empty() {
                return Err(format!("criterion {position}: rationale must not be blank"));
            }
            if entry.rationale.chars().count() > MAX_ACCEPTANCE_EVALUATION_RATIONALE_CHARS {
                return Err(format!(
                    "criterion {position}: rationale exceeds {MAX_ACCEPTANCE_EVALUATION_RATIONALE_CHARS} characters"
                ));
            }
            if entry.rationale.chars().any(char::is_control) {
                return Err(format!(
                    "criterion {position}: rationale must be a single line without control characters"
                ));
            }
            if entry.evidence.len() > MAX_ACCEPTANCE_EVALUATION_EVIDENCE_PER_CRITERION {
                return Err(format!(
                    "criterion {position}: at most {MAX_ACCEPTANCE_EVALUATION_EVIDENCE_PER_CRITERION} evidence locators are accepted"
                ));
            }
            if entry
                .evidence
                .iter()
                .any(|locator| !is_citable_acceptance_locator(locator))
            {
                return Err(format!(
                    "criterion {position}: an evidence locator is 8 to 64 lowercase hex characters, copied from the evidence list"
                ));
            }
            if verdict == "pass" && entry.evidence.is_empty() {
                return Err(format!(
                    "criterion {position}: a pass must cite at least one evidence locator; without proof the verdict is insufficient-evidence"
                ));
            }
        }
        Ok(())
    }

    /// Exactly one verdict for each of the task's criteria, by position.
    fn validate_coverage(&self, criteria_count: usize) -> std::result::Result<(), String> {
        let out_of_range = self
            .verdicts
            .iter()
            .map(|entry| entry.criterion)
            .find(|position| *position == 0 || *position > criteria_count);
        if let Some(position) = out_of_range {
            return Err(format!(
                "criterion {position} does not exist; the task has criteria 1 to {criteria_count}"
            ));
        }
        if self.verdicts.len() != criteria_count {
            return Err(format!(
                "the task has {criteria_count} criteria and each needs exactly one verdict; {} were given",
                self.verdicts.len()
            ));
        }
        Ok(())
    }
}

/// `anthropic/<model>` or `openai/<model>` for the tracker's `--model`, or
/// nothing when either segment would not be a clean bounded word.
fn acceptance_evaluator_model_flag(agent: Agent, model: &str) -> Option<String> {
    let provider = match agent {
        Agent::Claude => "anthropic",
        Agent::Codex => "openai",
        Agent::Cursor | Agent::Gemini | Agent::OpenCode | Agent::Kimi => return None,
    };
    let model = model
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();
    let model = model.trim();
    if model.is_empty() || model.len() > MAX_ACCEPTANCE_EVALUATION_MODEL_SEGMENT_BYTES {
        return None;
    }
    Some(format!("{provider}/{model}"))
}

/// `work … evaluate …` exactly as the evaluator's own session would run it.
/// The request must already have passed both validations.
fn acceptance_evaluation_cli_args(
    connection: &EngramConnectionConfig,
    target: &DelegationAcceptanceEvaluation,
    request: &SubmitAcceptanceEvaluationRequest,
    model: Option<&str>,
) -> std::result::Result<Vec<String>, ApiError> {
    // The stored ref is a positional argument: checked again where it is used,
    // whatever wrote the record.
    if !is_acceptance_evaluation_work_ref(&target.work_ref) {
        return Err(ApiError::conflict(
            "this evaluation's stored work ref cannot be passed to the tracker; request a new evaluation",
        ));
    }
    let mut args = vec![
        "work".to_owned(),
        "--actor-id".to_owned(),
        connection.actor_id.clone(),
        "--session-id".to_owned(),
        connection.session_id.clone(),
    ];
    if let Some(actor_context) = connection.actor_context.as_ref() {
        args.extend(["--actor-context".to_owned(), actor_context.clone()]);
    }
    args.extend([
        "evaluate".to_owned(),
        target.work_ref.clone(),
        "--mode".to_owned(),
        target.mode.word().to_owned(),
        "--acceptance-basis".to_owned(),
        target.acceptance_basis.to_string(),
        "--evidence-basis".to_owned(),
        target.evidence_basis.to_string(),
    ]);
    args.extend(acceptance_evaluation_verdict_args(request));
    if let Some(model) = model {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    args.extend([
        "--attempt".to_owned(),
        target.attempt_key.clone(),
        "--json".to_owned(),
    ]);
    Ok(args)
}

/// The evaluator's own part of the command: its verdicts, normalized and in
/// criterion order. Everything else in the list is the host's, and the host's
/// part can drift (a renamed developer, another model) while a write is open;
/// "the same verdicts" is decided on this part alone.
fn acceptance_evaluation_verdict_args(request: &SubmitAcceptanceEvaluationRequest) -> Vec<String> {
    let mut args = Vec::new();
    let mut verdicts = request.verdicts.iter().collect::<Vec<_>>();
    verdicts.sort_by_key(|entry| entry.criterion);
    for entry in verdicts {
        let position = entry.criterion;
        let verdict = acceptance_verdict_word(&entry.verdict).unwrap_or("insufficient_evidence");
        let basis = entry
            .basis
            .as_deref()
            .and_then(acceptance_basis_word)
            .unwrap_or("judgment");
        args.extend([
            "--verdict".to_owned(),
            format!("{position}={verdict}:{basis}"),
            "--rationale".to_owned(),
            format!("{position}={}", entry.rationale.trim()),
        ]);
        for locator in &entry.evidence {
            args.extend(["--evidence".to_owned(), format!("{position}={locator}")]);
        }
    }
    args
}

/// The actor id and context a stored argument list was built under, read back
/// from the layout `acceptance_evaluation_cli_args` writes. A resend of an open
/// write runs that list verbatim, so its environment must name the same actor.
fn acceptance_evaluation_args_identity(args: &[String]) -> Option<(String, Option<String>)> {
    let word = |index: usize| args.get(index).map(String::as_str);
    if (word(0), word(1), word(3)) != (Some("work"), Some("--actor-id"), Some("--session-id")) {
        return None;
    }
    let actor_id = args.get(2)?.clone();
    match (word(5), word(7)) {
        (Some("evaluate"), _) => Some((actor_id, None)),
        (Some("--actor-context"), Some("evaluate")) => Some((actor_id, Some(args.get(6)?.clone()))),
        _ => None,
    }
}

/// Names one exact argument list. While a write's outcome is open, only the
/// list with this digest may be sent again: the tracker replays it, and would
/// refuse or double-record anything else.
fn acceptance_evaluation_payload_digest(args: &[String]) -> String {
    let mut digest = Sha256::new();
    for arg in args {
        digest.update(arg.as_bytes());
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())
}

/// What one `engram work evaluate` run told the host about its write.
#[derive(Clone, Debug, PartialEq)]
enum AcceptanceEvaluationRunOutcome {
    /// Exit 0 with a JSON receipt: recorded (or replayed).
    Receipt(Value),
    /// Positive evidence that this run recorded nothing, which takes the exit
    /// code and the shape together: exit 1 with the tracker's error envelope
    /// (emitted only when the operation's transaction did not commit), or
    /// exit 2 with the argument parser's usage error (raised before any store
    /// is opened). Carries those words.
    Refused(String),
    /// The store stayed locked through the runner's retry: nothing recorded.
    Locked(String),
    /// The process never started, so this run sent nothing.
    NeverStarted(String),
    /// Everything else: a deadline, a transport failure after the process
    /// started, an unreadable receipt, a process that was killed or crashed,
    /// or a failure that does not say the transaction did not commit. The
    /// write may have landed.
    Unknown(String),
}

fn classify_acceptance_evaluation_run(
    result: std::result::Result<EngramCliOutput, EngramTransportError>,
) -> AcceptanceEvaluationRunOutcome {
    let output = match result {
        Ok(output) => output,
        Err(error) if error.process_never_started => {
            return AcceptanceEvaluationRunOutcome::NeverStarted(format!(
                "engram work evaluate: {error}"
            ));
        }
        Err(error) => {
            return AcceptanceEvaluationRunOutcome::Unknown(format!("engram work evaluate: {error}"));
        }
    };
    if output.success {
        return match serde_json::from_slice::<Value>(&output.stdout) {
            Ok(receipt) => AcceptanceEvaluationRunOutcome::Receipt(receipt),
            Err(error) => AcceptanceEvaluationRunOutcome::Unknown(format!(
                "engram work evaluate: the receipt was unreadable ({error})"
            )),
        };
    }
    let detail = truncate_chars(&output.failure_detail(), 4_000);
    // A process that was killed or crashed may have stopped after its commit.
    let Some(code) = output.exit.code() else {
        return AcceptanceEvaluationRunOutcome::Unknown(format!(
            "engram work evaluate ended abnormally ({}){}",
            output.status,
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    };
    if output.reports_locked_store() {
        return AcceptanceEvaluationRunOutcome::Locked(detail);
    }
    // Code and shape together, never one alone: a panic exits 101, and the
    // tracker's generic failure code also covers errors after the commit,
    // such as failing to print the receipt.
    match code {
        ACCEPTANCE_EVALUATION_REFUSAL_EXIT_CODE => {
            if let Some(message) = acceptance_evaluation_error_envelope_message(&output.stderr) {
                return AcceptanceEvaluationRunOutcome::Refused(truncate_chars(&message, 4_000));
            }
        }
        ACCEPTANCE_EVALUATION_USAGE_EXIT_CODE => {
            if is_acceptance_evaluation_usage_error(&output.stderr) {
                return AcceptanceEvaluationRunOutcome::Refused(detail);
            }
        }
        _ => {}
    }
    AcceptanceEvaluationRunOutcome::Unknown(if detail.is_empty() {
        format!("engram work evaluate exited with code {code} and said nothing")
    } else {
        format!(
            "engram work evaluate exited with code {code} without saying that nothing was recorded: {detail}"
        )
    })
}

/// The exit code Engram ends with when it prints its error envelope.
const ACCEPTANCE_EVALUATION_REFUSAL_EXIT_CODE: u8 = 1;
/// The exit code the argument parser ends with on a usage error.
const ACCEPTANCE_EVALUATION_USAGE_EXIT_CODE: u8 = 2;

/// Engram's refusal: `{"error":{"code":<word>,"message":…}}` on stderr, which
/// it prints only when the operation's transaction did not commit.
fn acceptance_evaluation_error_envelope_message(stderr: &[u8]) -> Option<String> {
    let envelope = serde_json::from_slice::<Value>(stderr).ok()?;
    let error = envelope.get("error")?;
    error
        .get("code")?
        .as_str()
        .filter(|code| !code.trim().is_empty())?;
    error.get("message")?.as_str().map(str::to_owned)
}

/// The argument parser's usage error (`error: …` then `Usage: …`), raised
/// before the tracker opens any store. The caller checks the exit code.
fn is_acceptance_evaluation_usage_error(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim_start();
    text.starts_with("error:") && text.lines().any(|line| line.starts_with("Usage:"))
}

/// The tracker's evidence window is byte-bounded, so long notes leave most of a
/// task's evidence behind the first page (two of six entries on the first live
/// task). `notes_window.after` continues it; each page holds older entries,
/// oldest first.
const MAX_ACCEPTANCE_EVIDENCE_PAGES: usize = 8;

fn acceptance_evidence_continuation(page: &Value) -> Option<String> {
    page.pointer("/notes_window/after")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

fn acceptance_evidence_page_len(page: &Value) -> usize {
    page.get("notes").and_then(Value::as_array).map_or(0, Vec::len)
}

/// Folds continuation pages into the first receipt: `notes` becomes every
/// collected entry oldest first, and `notes_omitted` what is still older than
/// the last page read.
fn merge_acceptance_evidence_pages(first: &mut Value, older_pages: Vec<Value>) {
    let Some(last) = older_pages.last() else {
        return;
    };
    let still_older = last
        .pointer("/notes_window/older")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut merged = Vec::new();
    for page in older_pages.iter().rev() {
        if let Some(notes) = page.get("notes").and_then(Value::as_array) {
            merged.extend(notes.iter().cloned());
        }
    }
    if let Some(notes) = first.get("notes").and_then(Value::as_array) {
        merged.extend(notes.iter().cloned());
    }
    first["notes"] = Value::Array(merged);
    first["notes_omitted"] = json!(still_older);
}

/// What the requesting agent needs from a spawned evaluation: the ids to wait
/// on and what was selected. The HTTP response also carries the whole child
/// session and the brief several times over, which an agent has no use for
/// and would pay for in context on every request. A same-session answer has
/// no delegation and is returned whole: its brief is the payload.
fn compact_acceptance_evaluation_request_result(response: &Value) -> Value {
    let Some(delegation) = response.get("delegation") else {
        return response.clone();
    };
    json!({
        "mode": response.get("mode"),
        "workRef": response.get("workRef"),
        "delegationId": delegation.get("id"),
        "childSessionId": delegation.get("childSessionId"),
        "agent": delegation.get("agent"),
        "model": delegation.get("model"),
        "status": delegation.get("status"),
        "acceptanceEvaluation": delegation.get("acceptanceEvaluation"),
        "notice": response.get("notice"),
        "next": "Wait with termal_resume_after_delegations for this delegationId; the fan-in says what the tracker recorded. Do not request another evaluation of the same task while this one runs.",
    })
}

const ACCEPTANCE_EVALUATION_UNKNOWN_OUTCOME_TEXT: &str = "the write outcome is unknown: the tracker may hold this evaluator's verdict; read the task before requesting another evaluation";

/// One line for the parent's fan-in: what the tracker accepted, which the
/// child's prose cannot stand in for. An open write is never reported as
/// "nothing": the tracker may hold it.
fn acceptance_evaluation_outcome_line(evaluation: &DelegationAcceptanceEvaluation) -> String {
    let subject = format!(
        "Acceptance evaluation of `{}` ({})",
        acceptance_brief_text(&evaluation.work_ref, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS),
        evaluation.mode.word()
    );
    match &evaluation.submission {
        AcceptanceEvaluationSubmission::None => format!(
            "{subject}: nothing was recorded; the tracker has no verdict from this evaluator."
        ),
        AcceptanceEvaluationSubmission::Pending { .. } => {
            format!("{subject}: {ACCEPTANCE_EVALUATION_UNKNOWN_OUTCOME_TEXT}.")
        }
        // Only a receipt ends the uncertainty, so whatever was last learned
        // (a refused resend included) is shown, not acted on.
        AcceptanceEvaluationSubmission::Unconfirmed { reason, .. } => format!(
            "{subject}: {ACCEPTANCE_EVALUATION_UNKNOWN_OUTCOME_TEXT}. Last learned: {}",
            acceptance_brief_text(reason, MAX_ACCEPTANCE_BRIEF_SUMMARY_CHARS)
        ),
        AcceptanceEvaluationSubmission::Recorded {
            receipt,
            recorded_at,
        } => format!(
            "{subject}: recorded at {recorded_at}{}. Read the task in the tracker for the verdicts.",
            acceptance_evaluation_receipt_summary(receipt)
        ),
    }
}

/// `passed` counts passing criteria out of `verdicts_total`, and `blocking`
/// names the first criterion that keeps the task from completing. An extract
/// of an unfamiliar receipt says nothing.
fn acceptance_evaluation_receipt_summary(receipt: &AcceptanceEvaluationReceiptExtract) -> String {
    let (Some(passed), Some(total)) = (receipt.passed, receipt.verdicts_total) else {
        return String::new();
    };
    if passed == total {
        return format!("; all {total} criteria passed");
    }
    let blocking = receipt
        .blocking
        .as_ref()
        .map(|blocking| {
            format!(
                "; criterion {} is {}",
                blocking.position,
                acceptance_brief_text(&blocking.verdict, MAX_ACCEPTANCE_BRIEF_LABEL_CHARS)
            )
        })
        .unwrap_or_default();
    format!("; {passed} of {total} criteria passed{blocking}, so the task cannot complete on it")
}

/// What is kept of the tracker's receipt, which nests the evaluation. Every
/// string is one bounded line; a field of an unfamiliar shape is left out.
fn acceptance_evaluation_receipt_extract(receipt: &Value) -> AcceptanceEvaluationReceiptExtract {
    let Some(evaluation) = receipt.get("evaluation") else {
        return AcceptanceEvaluationReceiptExtract::default();
    };
    // A hard cut with no marker: these are identifiers and words, not prose.
    let bounded = |value: &str, max_chars: usize| {
        acceptance_brief_line(value)
            .chars()
            .take(max_chars)
            .collect::<String>()
    };
    let word = |key: &str, max_chars: usize| {
        evaluation
            .get(key)
            .and_then(Value::as_str)
            .map(|value| bounded(value, max_chars))
    };
    AcceptanceEvaluationReceiptExtract {
        evaluation_hash: word("hash", MAX_ACCEPTANCE_RECEIPT_HASH_CHARS),
        mode: word("mode", MAX_ACCEPTANCE_RECEIPT_WORD_CHARS),
        passed: evaluation.get("passed").and_then(Value::as_u64),
        verdicts_total: evaluation.get("verdicts_total").and_then(Value::as_u64),
        blocking: evaluation.get("blocking").and_then(|blocking| {
            Some(AcceptanceEvaluationBlockingVerdict {
                position: blocking.get("position")?.as_u64()?,
                verdict: bounded(
                    blocking.get("verdict")?.as_str()?,
                    MAX_ACCEPTANCE_RECEIPT_WORD_CHARS,
                ),
            })
        }),
        replayed: evaluation
            .get("replayed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        work_revision: evaluation.get("work_revision").and_then(Value::as_i64),
        evaluated_cut: evaluation.get("evaluated_cut").and_then(Value::as_i64),
    }
}

/// The raw receipt for the evaluator's own response: whole when it is small,
/// otherwise its leading bytes as text, with the cut said.
fn bounded_acceptance_evaluation_receipt(receipt: Value) -> (Value, bool) {
    let encoded = receipt.to_string();
    if encoded.len() <= MAX_ACCEPTANCE_SUBMIT_RESPONSE_RECEIPT_BYTES {
        return (receipt, false);
    }
    let mut end = MAX_ACCEPTANCE_SUBMIT_RESPONSE_RECEIPT_BYTES;
    while !encoded.is_char_boundary(end) {
        end -= 1;
    }
    (Value::String(encoded[..end].to_owned()), true)
}
