// Durable evaluator-attempt ownership. Reuses the existing request preparation
// and submission fences; it does not infer tracker standing from host bindings.
const MAX_ACCEPTANCE_EVALUATOR_ATTEMPTS: u8 = 3;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationAttemptHistory {
    schema_version: u32,
    parent_session_id: String,
    child_session_id: String,
    cwd: String,
    identity: AcceptanceEvidenceIdentity,
    ordinal: u8,
    #[serde(default)]
    requester_text_tainted: bool,
    #[serde(default)]
    identity_refused: bool,
    #[serde(default)]
    previous: Vec<AcceptanceEvaluationPreviousAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refusal: Option<AcceptanceEvaluationDefinitiveRefusal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prepared_brief: Option<AcceptanceEvaluationPreparedBrief>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationPreviousAttempt {
    ordinal: u8,
    key: String,
    acceptance_basis: i64,
    evidence_basis: i64,
    source_fingerprint: Option<String>,
    submission: AcceptanceEvaluationSubmission,
    refusal: Option<AcceptanceEvaluationDefinitiveRefusal>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationDefinitiveRefusal {
    code: String,
    message: String,
    original: AcceptanceEvaluationOpenWrite,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationPreparedBrief {
    attempt_key: String,
    prompt: String,
    digest: String,
    /// A retained brief is not permission to mint another key. Only an echoed
    /// key or the exact prompt's admitted follow-up proves its consumption.
    #[serde(default)]
    offered: bool,
}

fn acceptance_evaluation_ordinal_key(delegation_id: &str, ordinal: u8) -> String {
    format!("{delegation_id}:attempt:{ordinal}")
}

fn acceptance_evaluation_attempt_prompt(
    prompt: &str,
    key: &str,
    ordinal: u8,
    previous: Option<&DelegationAcceptanceEvaluation>,
    seed: &AcceptanceEvaluationTargetSeed,
) -> String {
    let mut header = format!(
        "Host-authored acceptance attempt {ordinal}.\nAttempt key: {key}\nCurrent acceptance basis: {}. Current evidence cut: {}.\nEcho this exact key as attemptKey in your submission. Judge every criterion afresh; earlier verdicts are not adopted.\n",
        seed.acceptance_basis, seed.evidence_basis
    );
    if let Some(previous) = previous {
        header.push_str(&format!(
            "Previous evidence cut: {}. The brief below names the newly read cut and all available evidence through it. Changes are presentation, never a restriction of judgment scope.\nPrevious source: {}; current source: {}.\n",
            previous.evidence_basis,
            acceptance_brief_text(previous.source_fingerprint.as_deref().unwrap_or("unmeasured"), 160),
            acceptance_brief_text(seed.source_fingerprint.as_deref().unwrap_or("unmeasured"), 160)
        ));
        if previous.source_fingerprint != seed.source_fingerprint {
            header.push_str("Source changed. Inspect the whole new revision and judge every criterion afresh; bound criteria need checks passed on that revision.\n");
        }
        if let Some(refusal) = previous
            .attempt_history
            .as_ref()
            .and_then(|h| h.refusal.as_ref())
        {
            header.push_str(&format!(
                "Earlier confirmed refusal: {}: {}\n",
                acceptance_brief_text(&refusal.code, 96),
                acceptance_brief_text(&refusal.message, 512)
            ));
        }
    }
    format!("{header}\n{prompt}")
}
