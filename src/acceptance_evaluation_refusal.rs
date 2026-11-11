// Terminal attempt ownership. A later refusal cannot resolve an unknown send.
// Local retirement ends retries while retaining the original unknown write;
// it cannot manufacture a producer refusal or a submitted judgment.
const MAX_ACCEPTANCE_REFUSED_SUBMISSIONS: usize = MAX_ACCEPTANCE_EVALUATOR_ATTEMPTS as usize * 2;

/// A bounded requester view, not an Engram evaluation or an admission receipt.
/// Contains only the evaluator's submitted judgment and its host-owned cuts.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceEvaluationRefusedSubmission {
    attempt_key: String,
    ordinal: u8,
    acceptance_basis: i64,
    evidence_basis: i64,
    code: String,
    message: String,
    /// Always false, including when loading an older snapshot.
    #[serde(skip_deserializing)]
    recorded: bool,
    #[serde(flatten)]
    judgment: AcceptanceEvaluationRefusedJudgment,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum AcceptanceEvaluationRefusedJudgment {
    Refused {
        verdicts: Vec<SubmitAcceptanceEvaluationVerdict>,
    },
    /// Old records did not retain a safe payload. Never parse private argv to
    /// reconstruct it, or mistake the absence of a payload for no refusal.
    Unavailable,
}

fn deserialize_acceptance_refused_submissions<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<AcceptanceEvaluationRefusedSubmission>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries = Vec::<AcceptanceEvaluationRefusedSubmission>::deserialize(deserializer)?;
    if entries.len() > MAX_ACCEPTANCE_REFUSED_SUBMISSIONS {
        return Err(serde::de::Error::custom(
            "too many refused evaluation submissions",
        ));
    }
    for entry in &entries {
        if let AcceptanceEvaluationRefusedJudgment::Refused { verdicts } = &entry.judgment {
            SubmitAcceptanceEvaluationRequest {
                schema_version: ACCEPTANCE_EVALUATION_SUBMISSION_SCHEMA_VERSION,
                attempt_key: None,
                verdicts: verdicts.clone(),
            }
            .validate_shape()
            .map_err(serde::de::Error::custom)?;
        }
    }
    Ok(entries)
}

impl AcceptanceEvaluationRefusedSubmission {
    fn new(target: &DelegationAcceptanceEvaluation, code: Option<&str>, message: &str) -> Self {
        Self {
            attempt_key: target.attempt_key.clone(),
            ordinal: target
                .attempt_history
                .as_ref()
                .map_or(0, |history| history.ordinal),
            acceptance_basis: target.acceptance_basis,
            evidence_basis: target.evidence_basis,
            code: acceptance_brief_text(code.unwrap_or("usage_error"), 96),
            message: truncate_chars(message, 4_000),
            recorded: false,
            judgment: AcceptanceEvaluationRefusedJudgment::Unavailable,
        }
    }
}

impl AcceptanceEvaluationAttemptHistory {
    /// Only client copies receive unavailable placeholders for pre-change
    /// refusals. No backfill or private command interpretation is performed.
    fn expose_unavailable_refused_submissions(&mut self, target: &DelegationAcceptanceEvaluation) {
        let mut missing = Vec::new();
        let mut keep =
            |key: &str, ordinal, acceptance_basis, evidence_basis, code: &str, message: &str| {
                // A local retirement retains uncertainty in attempt history;
                // it is not a producer refusal with a missing payload.
                if code == ACCEPTANCE_LOCAL_RETIREMENT_CODE {
                    return;
                }
                let mut entry =
                    AcceptanceEvaluationRefusedSubmission::new(target, Some(code), message);
                entry.attempt_key = key.to_owned();
                entry.ordinal = ordinal;
                entry.acceptance_basis = acceptance_basis;
                entry.evidence_basis = evidence_basis;
                if !self.refused_submissions.iter().chain(missing.iter()).any(
                    |kept: &AcceptanceEvaluationRefusedSubmission| {
                        kept.attempt_key == entry.attempt_key
                            && kept.code == entry.code
                            && kept.message == entry.message
                    },
                ) {
                    missing.push(entry);
                }
            };
        if let Some(refusal) = &self.refusal {
            keep(
                &target.attempt_key,
                self.ordinal,
                target.acceptance_basis,
                target.evidence_basis,
                &refusal.code,
                &refusal.message,
            );
        }
        if let Some(retry) = &self.citation_retry {
            keep(
                &target.attempt_key,
                self.ordinal,
                target.acceptance_basis,
                target.evidence_basis,
                &retry.code,
                &retry.message,
            );
        }
        for previous in &self.previous {
            if let Some(refusal) = &previous.refusal {
                keep(
                    &previous.key,
                    previous.ordinal,
                    previous.acceptance_basis,
                    previous.evidence_basis,
                    &refusal.code,
                    &refusal.message,
                );
            }
        }
        self.refused_submissions.extend(missing.into_iter().take(
            MAX_ACCEPTANCE_REFUSED_SUBMISSIONS.saturating_sub(self.refused_submissions.len()),
        ));
    }
}

impl AppState {
    /// All send paths retire stored revision declarations through this owner.
    /// The history retains the exact unknown write, never a producer refusal.
    fn retire_legacy_acceptance_submission(
        &self,
        authority: &AcceptanceEvaluationSubmitAuthority,
    ) -> Result<(), ApiError> {
        let original = match &authority.target.submission {
            AcceptanceEvaluationSubmission::Pending { original, .. }
            | AcceptanceEvaluationSubmission::Unconfirmed { original, .. } => original,
            _ => return Ok(()),
        };
        if !original.args.iter().any(|arg| {
            arg == "--source-fingerprint" || arg.starts_with("--source-fingerprint=")
        }) {
            return Ok(());
        }
        const RETIRED: &str = "The retained source-fingerprint submission is retired locally and will not be sent again. Any earlier tracker outcome remains unknown; finish and report this attempt, and read the tracker before requesting a new evaluation.";
        self.record_acceptance_evaluation_disposition(
            authority,
            Some(ACCEPTANCE_LOCAL_RETIREMENT_CODE),
            RETIRED,
            original.clone(),
            Vec::new(),
            None,
        )?;
        Err(ApiError::conflict(RETIRED))
    }

    /// Settles a confirmed refusal of this attempt. One that names citable
    /// alternatives, on an attempt that has not used its re-submission, is
    /// kept as that retry instead of as definitive, and `true` says so
    /// (acceptance_evaluation_citation_retry.rs).
    fn record_acceptance_evaluation_refusal(
        &self,
        authority: &AcceptanceEvaluationSubmitAuthority,
        request: &SubmitAcceptanceEvaluationRequest,
        code: Option<&str>,
        message: &str,
        original: AcceptanceEvaluationOpenWrite,
        alternatives: Vec<AcceptanceCitableAlternatives>,
    ) -> Result<bool, ApiError> {
        // This function is reached only on a confirmed refusal with no earlier
        // uncertain send. The request is the already validated payload actually
        // sent, before host attribution was prefixed to tracker rationales.
        let mut refused =
            AcceptanceEvaluationRefusedSubmission::new(&authority.target, code, message);
        let mut verdicts = request.verdicts.clone();
        verdicts.sort_by_key(|entry| entry.criterion);
        for entry in &mut verdicts {
            entry.verdict = acceptance_verdict_word(&entry.verdict)
                .ok_or_else(|| {
                    ApiError::internal("confirmed refusal has an invalid submitted verdict")
                })?
                .to_owned();
            entry.basis = Some(
                acceptance_basis_word(entry.basis.as_deref().unwrap_or("judgment"))
                    .ok_or_else(|| {
                        ApiError::internal("confirmed refusal has an invalid submitted basis")
                    })?
                    .to_owned(),
            );
            entry.rationale = entry.rationale.trim().to_owned();
        }
        refused.judgment = AcceptanceEvaluationRefusedJudgment::Refused { verdicts };
        self.record_acceptance_evaluation_disposition(
            authority,
            code,
            message,
            original,
            alternatives,
            Some(refused),
        )
    }

    /// Persists the existing terminal-attempt state under its original fences.
    /// Only a confirmed producer refusal supplies a refused judgment; local
    /// retirement retains the unknown original without projecting a new one.
    fn record_acceptance_evaluation_disposition(
        &self,
        authority: &AcceptanceEvaluationSubmitAuthority,
        code: Option<&str>,
        message: &str,
        original: AcceptanceEvaluationOpenWrite,
        alternatives: Vec<AcceptanceCitableAlternatives>,
        refused: Option<AcceptanceEvaluationRefusedSubmission>,
    ) -> Result<bool, ApiError> {
        let (written, dispatch, retry) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index =
                acceptance_evaluation_attempt_index_locked(&inner, authority).ok_or_else(|| {
                    ApiError::conflict("the refused attempt no longer belongs to this delegation")
                })?;
            let target = inner.delegations[index]
                .acceptance_evaluation
                .as_mut()
                .unwrap();
            let history =
                target
                    .attempt_history
                    .get_or_insert_with(|| AcceptanceEvaluationAttemptHistory {
                        // No legacy record becomes eligible merely because it refused.
                        schema_version: 0,
                        parent_session_id: String::new(),
                        child_session_id: String::new(),
                        cwd: String::new(),
                        identity: AcceptanceEvidenceIdentity::default(),
                        ordinal: 0,
                        requester_text_tainted: true,
                        identity_refused: false,
                        previous: Vec::new(),
                        refusal: None,
                        prepared_brief: None,
                        citation_retry: None,
                        refused_submissions: Vec::new(),
                    });
            if let Some(refused) = refused {
                if history.refused_submissions.len() >= MAX_ACCEPTANCE_REFUSED_SUBMISSIONS
                    || history
                        .refused_submissions
                        .iter()
                        .filter(|entry| entry.attempt_key == refused.attempt_key)
                        .count()
                        >= 2
                {
                    return Err(ApiError::internal(
                        "confirmed refusal cannot fit its retained submission history",
                    ));
                }
                history.refused_submissions.push(refused);
            }
            let retry = acceptance_citation_retry_allowed(history, code, &alternatives);
            if retry {
                history.citation_retry = Some(AcceptanceEvaluationCitationRetry {
                    code: code.unwrap_or_default().to_owned(),
                    message: message.to_owned(),
                    alternatives,
                });
            } else {
                history.refusal = Some(AcceptanceEvaluationDefinitiveRefusal {
                    code: code.unwrap_or("usage_error").to_owned(),
                    message: message.to_owned(),
                    original,
                });
            }
            history.identity_refused |= code.is_some() && message.contains("independent_session evaluation must come from a session that neither holds nor executes the run");
            target.submission = AcceptanceEvaluationSubmission::None;
            inner.mark_delegation_mutated(index);
            let (_, dispatch) = self
                .commit_locked_with_persist_dispatch(&mut inner)
                .map_err(|e| {
                    ApiError::internal(format!(
                        "terminal attempt disposition retained; its local acknowledgement is unknown: {e:#}"
                    ))
                })?;
            (inner.delegations[index].clone(), dispatch, retry)
        };
        if dispatch != PersistDispatch::Synchronous {
            self.confirm_acceptance_evaluation_submission_durable(authority, &written)
                .map_err(|e| {
                    ApiError::internal(format!(
                        "terminal attempt disposition retained; its local acknowledgement is unknown: {e}"
                    ))
                })?;
        }
        Ok(retry)
    }
}
