// The one re-submission a refused acceptance evaluation may make when Engram's
// off-source refusal names records it admits instead.
// Owns: reading those citable alternatives from the tracker's refusal, the
// persisted record of the retry they allow, the evaluator's answer that offers
// it, and the local refusal of a re-submission citing anything else for the
// criteria they cover.
// Does not own: deciding that a refusal is definitive, the stored refusal or
// its persistence (acceptance_evaluation_refusal.rs), the typed resubmit and
// its refreshed brief (acceptance_evaluation_api.rs), or which records Engram
// admits, which the tracker alone decides.
// A new module beside acceptance_evaluation_refusal.rs, included from main.rs.

/// The refusal code Engram gives a citation it does not admit for a bound
/// criterion, among other admission refusals; only on this code are
/// alternatives read.
const ACCEPTANCE_EVALUATION_ADMISSION_REFUSAL_CODE: &str = "acceptance_evaluation_refused";

/// At most this many records are kept for one criterion; the first ones are
/// as citable as the rest. Engram names at most four.
const MAX_ACCEPTANCE_CITABLE_ALTERNATIVES_PER_CRITERION: usize = 8;

/// The records Engram names as citable for the criterion an off-source
/// refusal concerns (`error.details.cause`: `criterion` and
/// `citable_records.records`, newest first; absent when none is citable).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceCitableAlternatives {
    criterion: usize,
    records: Vec<String>,
}

/// A refusal that allowed one re-submission, kept on the attempt: while it is
/// there the attempt may submit once more, citing for each criterion it names
/// only the records named for it; a further refusal is definitive.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationCitationRetry {
    code: String,
    message: String,
    alternatives: Vec<AcceptanceCitableAlternatives>,
}

/// The citable alternatives in Engram's refusal envelope: for the one
/// criterion its cause names, only the record ids the evaluator could cite,
/// each once. Anything of another shape gives none, so a malformed cause
/// allows no re-submission. A list, so that a cause naming several criteria
/// could be carried without a change of the retry it keys.
fn acceptance_citable_alternatives(envelope: &Value) -> Vec<AcceptanceCitableAlternatives> {
    let Some(cause) = envelope.pointer("/error/details/cause") else {
        return Vec::new();
    };
    let Some(criterion) = cause
        .get("criterion")
        .and_then(Value::as_u64)
        .and_then(|criterion| usize::try_from(criterion).ok())
        .filter(|criterion| (1..=MAX_ACCEPTANCE_EVALUATION_VERDICTS).contains(criterion))
    else {
        return Vec::new();
    };
    let mut records: Vec<String> = Vec::new();
    for record in cause
        .pointer("/citable_records/records")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|record| is_citable_acceptance_locator(record))
    {
        if records.len() < MAX_ACCEPTANCE_CITABLE_ALTERNATIVES_PER_CRITERION
            && !records.iter().any(|kept| kept == record)
        {
            records.push(record.to_owned());
        }
    }
    if records.is_empty() {
        return Vec::new();
    }
    vec![AcceptanceCitableAlternatives { criterion, records }]
}

/// Whether this refusal allows the attempt's one re-submission: an admission
/// refusal naming alternatives, on an attempt that has not used it.
fn acceptance_citation_retry_allowed(
    history: &AcceptanceEvaluationAttemptHistory,
    code: Option<&str>,
    alternatives: &[AcceptanceCitableAlternatives],
) -> bool {
    code == Some(ACCEPTANCE_EVALUATION_ADMISSION_REFUSAL_CODE)
        && !alternatives.is_empty()
        && history.citation_retry.is_none()
}

/// "criterion 2: aaaa…, bbbb…; criterion 3: cccc…".
fn acceptance_citable_alternatives_text(alternatives: &[AcceptanceCitableAlternatives]) -> String {
    alternatives
        .iter()
        .map(|entry| format!("criterion {}: {}", entry.criterion, entry.records.join(", ")))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The evaluator's answer when the refusal allowed a re-submission.
fn acceptance_citation_retry_offer(words: &str, alternatives: &[AcceptanceCitableAlternatives]) -> String {
    format!(
        "Engram refused the evaluation; nothing was recorded: {words}. It names records it admits instead ({}). You may submit once more under the same attemptKey: for each criterion named here cite only the records named for it, after checking them against the criterion. Any further refusal is final: finish and report it",
        acceptance_citable_alternatives_text(alternatives)
    )
}

impl SubmitAcceptanceEvaluationRequest {
    /// The re-submission a refusal allowed cites, for each criterion it named,
    /// only the records it named for that criterion.
    fn validate_citation_retry(
        &self,
        retry: &AcceptanceEvaluationCitationRetry,
    ) -> std::result::Result<(), String> {
        for entry in &self.verdicts {
            let Some(allowed) = retry
                .alternatives
                .iter()
                .find(|alternatives| alternatives.criterion == entry.criterion)
            else {
                continue;
            };
            if let Some(other) = entry
                .evidence
                .iter()
                .find(|locator| !allowed.records.contains(locator))
            {
                return Err(format!(
                    "criterion {}: this re-submission may cite for it only the records Engram named ({}); {other} is not one of them",
                    entry.criterion,
                    allowed.records.join(", ")
                ));
            }
        }
        Ok(())
    }
}
