// Grant-free execution_observe requests for a turn the host observed only
// after it began: an unadmitted turn (its measured source change or none, and
// up to 16 observed checks) and a standalone observed check of such a turn,
// with the receipt check for both. Every check is uncredited by construction;
// causality is unknown. Building mirrors the producer's own shape rules
// (Engram's docs/features/behavioral-control-plane.md, execution_observe):
// a request that breaks a bound or a relation between its facts is refused
// whole, never truncated, so what is sent is exactly what was observed.
//
// Owns the observed-check type, the two request builders and the unadmitted
// receipt validation. Does not own the inter-turn change request or its
// receipt (`engram_source_observation_protocol.rs`), when a runtime-started
// turn is reported or how its facts are measured, persisted and recovered
// (the runtime-started turn path), or splitting more than 16 checks into
// standalone requests (the caller's). New module beside the protocol file,
// created instead of growing it. Until the runtime-started turn path sends
// these requests, only this module's tests build them, so its items allow
// dead code outside tests.

#[cfg_attr(not(test), allow(dead_code))]
const ENGRAM_OBSERVATION_MAX_CHECKS: usize = 16;
#[cfg_attr(not(test), allow(dead_code))]
const ENGRAM_OBSERVATION_MAX_LABEL_BYTES: usize = 128;
#[cfg_attr(not(test), allow(dead_code))]
const ENGRAM_OBSERVATION_MAX_EVIDENCE_REF_BYTES: usize = 2_048;
#[cfg_attr(not(test), allow(dead_code))]
const ENGRAM_OBSERVATION_MAX_SOURCE_BYTES: usize = 512;
#[cfg_attr(not(test), allow(dead_code))]
const ENGRAM_OBSERVATION_MAX_REQUEST_BYTES: usize = 64 * 1_024;

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramObservedCheckResult {
    Passed,
    Failed,
    Indeterminate,
}

#[cfg_attr(not(test), allow(dead_code))]
/// One check the host saw run inside an unadmitted turn: a bounded fact,
/// never verification evidence. A passed or failed result has finished.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EngramObservedCheck {
    host_check_id: String,
    check_kind: EngramVerificationKind,
    observed_result: EngramObservedCheckResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_at: Option<String>,
    observed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_basis: Option<EngramExecutionSourceBasis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host_evidence_ref: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
/// The facts every unadmitted occurrence carries besides the occurrence
/// itself: the exact binding and canonical root basis at the capture cut,
/// the window actually observed and the accounting policy.
#[derive(Clone, Debug)]
struct EngramUnadmittedObservationBasis {
    idempotency_key: String,
    binding: EngramControlWorkBinding,
    root_basis: EngramObservationRootBasis,
    observed_interval: EngramObservationInterval,
    policy_basis: EngramObservationPolicyBasis,
}

#[cfg_attr(not(test), allow(dead_code))]
/// The execution_observe request for an unadmitted turn, validated as built
/// at `built_at`.
fn engram_unadmitted_turn_request(
    routing_token: &str,
    basis: EngramUnadmittedObservationBasis,
    host_turn_ref: String,
    source_change: Option<EngramObservedSourceChange>,
    observed_checks: Vec<EngramObservedCheck>,
    built_at: chrono::DateTime<chrono::Utc>,
) -> Result<EngramControlRequest, EngramTransportError> {
    engram_unadmitted_request(
        routing_token,
        basis,
        EngramInterTurnOccurrence::UnadmittedTurn { host_turn_ref, source_change, observed_checks },
        built_at,
    )
}

#[cfg_attr(not(test), allow(dead_code))]
/// The execution_observe request for one check of an unadmitted turn,
/// reported on its own, validated as built at `built_at`.
fn engram_observed_check_request(
    routing_token: &str,
    basis: EngramUnadmittedObservationBasis,
    host_turn_ref: String,
    check: EngramObservedCheck,
    built_at: chrono::DateTime<chrono::Utc>,
) -> Result<EngramControlRequest, EngramTransportError> {
    engram_unadmitted_request(
        routing_token,
        basis,
        EngramInterTurnOccurrence::ObservedCheck { host_turn_ref, check },
        built_at,
    )
}

#[cfg_attr(not(test), allow(dead_code))]
fn engram_unadmitted_request(
    routing_token: &str,
    basis: EngramUnadmittedObservationBasis,
    occurrence: EngramInterTurnOccurrence,
    built_at: chrono::DateTime<chrono::Utc>,
) -> Result<EngramControlRequest, EngramTransportError> {
    let observation = EngramInterTurnObservation {
        idempotency_key: basis.idempotency_key,
        binding: basis.binding,
        root_basis: basis.root_basis,
        observed_interval: basis.observed_interval,
        occurrence,
        causality: EngramObservationCausality::Unknown {},
        policy_basis: basis.policy_basis,
    };
    engram_validate_unadmitted_observation(&observation, built_at)
        .map_err(|reason| EngramTransportError::protocol(format!("unadmitted observation refused: {reason}")))?;
    let request = EngramControlRequest::ExecutionObserve {
        routing_token: routing_token.to_owned(),
        observation,
    };
    let bytes = serde_json::to_vec(&request).map_err(|_| {
        EngramTransportError::protocol("unadmitted observation refused: the request could not serialize")
    })?;
    if bytes.len() > ENGRAM_OBSERVATION_MAX_REQUEST_BYTES {
        return Err(EngramTransportError::protocol(format!(
            "unadmitted observation refused: the request is {} bytes, over its {ENGRAM_OBSERVATION_MAX_REQUEST_BYTES}-byte bound",
            bytes.len()
        )));
    }
    Ok(request)
}

#[cfg_attr(not(test), allow(dead_code))]
fn engram_observation_instant(value: &str, label: &str) -> Result<chrono::DateTime<chrono::Utc>, String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|at| at.with_timezone(&chrono::Utc))
        .map_err(|_| format!("{label} is not an RFC 3339 time"))
}

#[cfg_attr(not(test), allow(dead_code))]
fn engram_observation_text(value: &str, max_bytes: usize, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.trim() != value || value.len() > max_bytes {
        return Err(format!("{label} must be trimmed, nonblank and at most {max_bytes} bytes"));
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
fn engram_observation_within(
    at: &str,
    from: chrono::DateTime<chrono::Utc>,
    through: chrono::DateTime<chrono::Utc>,
    label: &str,
) -> Result<chrono::DateTime<chrono::Utc>, String> {
    let at = engram_observation_instant(at, label)?;
    if at < from || at > through {
        return Err(format!("{label} lies outside observed_interval"));
    }
    Ok(at)
}

#[cfg_attr(not(test), allow(dead_code))]
fn engram_observation_source_basis(basis: &EngramExecutionSourceBasis, label: &str) -> Result<(), String> {
    engram_observation_text(&basis.workspace_id, ENGRAM_OBSERVATION_MAX_SOURCE_BYTES, &format!("{label}.workspace_id"))?;
    engram_observation_text(&basis.source_revision, ENGRAM_OBSERVATION_MAX_SOURCE_BYTES, &format!("{label}.source_revision"))?;
    if !matches!(
        (basis.source_root_generation, basis.source_root_state),
        (None, None) | (Some(1..), Some(_))
    ) {
        return Err(format!("{label} names a root generation and state only together, the generation positive"));
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
/// The producer's shape rules for a grant-free occurrence, checked before
/// sending: each bound, the relations between the facts, and every time
/// inside the observed window, which ends no later than `built_at`.
fn engram_validate_unadmitted_observation(
    observation: &EngramInterTurnObservation,
    built_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    engram_observation_text(&observation.idempotency_key, ENGRAM_OBSERVATION_MAX_LABEL_BYTES, "idempotency_key")?;
    if observation.binding.work_revision < 1 || observation.binding.claim_fence < 1 {
        return Err("binding work_revision and claim_fence must be positive".into());
    }
    // A policy built in code is not decoded, so its record ids are checked
    // here as the inter-turn builder checks them.
    let EngramObservationPolicyBasis::AccountIfEligible { project_policy_epoch, policy, obligation_rule_set } =
        &observation.policy_basis;
    if *project_policy_epoch < 1 || !engram_observation_object_id(policy)
        || !engram_observation_object_id(obligation_rule_set)
    {
        return Err("policy_basis needs a positive epoch and record ids for its policy and rule set".into());
    }
    let root = &observation.root_basis;
    if root.capture_run_cut < 1 || root.latest_event.as_ref().is_some_and(|id| !engram_observation_object_id(id)) {
        return Err("root_basis needs a positive capture cut and a record id or none".into());
    }
    let from = engram_observation_instant(&observation.observed_interval.from, "observed_interval.from")?;
    let through = engram_observation_instant(&observation.observed_interval.through, "observed_interval.through")?;
    if from > through {
        return Err("observed_interval.from is after observed_interval.through".into());
    }
    if through > built_at {
        return Err("observed_interval.through is after the request was built".into());
    }
    let (host_turn_ref, source_change, checks) = match &observation.occurrence {
        EngramInterTurnOccurrence::UnadmittedTurn { host_turn_ref, source_change, observed_checks } =>
            (host_turn_ref, source_change.as_ref(), observed_checks.as_slice()),
        EngramInterTurnOccurrence::ObservedCheck { host_turn_ref, check } =>
            (host_turn_ref, None, std::slice::from_ref(check)),
        EngramInterTurnOccurrence::InterTurnChange { .. } =>
            return Err("an inter-turn change is not an unadmitted occurrence".into()),
    };
    engram_observation_text(host_turn_ref, ENGRAM_OBSERVATION_MAX_LABEL_BYTES, "host_turn_ref")?;
    if let Some(change) = source_change {
        engram_validate_observed_source_change(change, from, through)?;
    }
    engram_validate_observed_checks(checks, from, through)
}

#[cfg_attr(not(test), allow(dead_code))]
fn engram_validate_observed_source_change(
    change: &EngramObservedSourceChange,
    from: chrono::DateTime<chrono::Utc>,
    through: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    match change {
        EngramObservedSourceChange::ContentComparison { workspace_id, baseline, sighting } => {
            engram_observation_text(workspace_id, ENGRAM_OBSERVATION_MAX_SOURCE_BYTES, "source_change.workspace_id")?;
            engram_observation_text(&baseline.workspace_id, ENGRAM_OBSERVATION_MAX_SOURCE_BYTES, "source_change.baseline.workspace_id")?;
            engram_observation_text(&baseline.source_revision, ENGRAM_OBSERVATION_MAX_SOURCE_BYTES, "source_change.baseline.source_revision")?;
            engram_observation_source_basis(&sighting.source_basis, "source_change.sighting.source_basis")?;
            if &baseline.workspace_id != workspace_id || &sighting.source_basis.workspace_id != workspace_id {
                return Err("a measured source change names one workspace throughout".into());
            }
            if baseline.source_revision == sighting.source_basis.source_revision {
                return Err("content_comparison needs two different revisions".into());
            }
            let seen_before = engram_observation_within(&baseline.observed_at, from, through, "source_change.baseline")?;
            let seen_after = engram_observation_within(&sighting.observed_at, from, through, "source_change.sighting")?;
            if seen_before > seen_after {
                return Err("source_change.baseline was observed after its sighting".into());
            }
        }
        EngramObservedSourceChange::AssumedMissingBaseline { workspace_id, sighting } => {
            engram_observation_text(workspace_id, ENGRAM_OBSERVATION_MAX_SOURCE_BYTES, "source_change.workspace_id")?;
            engram_observation_source_basis(&sighting.source_basis, "source_change.sighting.source_basis")?;
            if &sighting.source_basis.workspace_id != workspace_id {
                return Err("a measured source change names one workspace throughout".into());
            }
            engram_observation_within(&sighting.observed_at, from, through, "source_change.sighting")?;
        }
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
fn engram_validate_observed_checks(
    checks: &[EngramObservedCheck],
    from: chrono::DateTime<chrono::Utc>,
    through: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    if checks.len() > ENGRAM_OBSERVATION_MAX_CHECKS {
        return Err(format!(
            "an unadmitted turn carries at most {ENGRAM_OBSERVATION_MAX_CHECKS} observed checks, not {}",
            checks.len()
        ));
    }
    let mut ids = std::collections::HashSet::new();
    for check in checks {
        engram_observation_text(&check.host_check_id, ENGRAM_OBSERVATION_MAX_LABEL_BYTES, "observed check host_check_id")?;
        if !ids.insert(check.host_check_id.as_str()) {
            return Err("an observed check id appears twice in one request".into());
        }
        let label = format!("observed check {:?}", check.host_check_id);
        let started = check.started_at.as_deref()
            .map(|at| engram_observation_within(at, from, through, &format!("{label} started_at")))
            .transpose()?;
        let finished = check.finished_at.as_deref()
            .map(|at| engram_observation_within(at, from, through, &format!("{label} finished_at")))
            .transpose()?;
        engram_observation_within(&check.observed_at, from, through, &format!("{label} observed_at"))?;
        if let (Some(started), Some(finished)) = (started, finished)
            && started > finished
        {
            return Err(format!("{label} started after it finished"));
        }
        if matches!(check.observed_result, EngramObservedCheckResult::Passed | EngramObservedCheckResult::Failed)
            && finished.is_none()
        {
            return Err(format!("{label} reports a finished result without finished_at; an unfinished check is indeterminate"));
        }
        if let Some(basis) = &check.source_basis {
            engram_observation_source_basis(basis, &format!("{label} source_basis"))?;
        }
        if let Some(reference) = &check.host_evidence_ref {
            engram_observation_text(reference, ENGRAM_OBSERVATION_MAX_EVIDENCE_REF_BYTES, &format!("{label} host_evidence_ref"))?;
        }
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
/// One check's summary on a receipt: its id and the producer-set credit.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct EngramObservedCheckSummary {
    host_check_id: String,
    credit: String,
}

impl EngramSourceObservationReceipt {
    #[cfg_attr(not(test), allow(dead_code))]
    /// Decodes the receipt of an unadmitted occurrence built here: it must
    /// record exactly the request under its scope, name exactly the request's
    /// checks, each uncredited, and account for a reported change as a change
    /// (or a repeat), for no reported change as no change, or keep the record
    /// for a terminal historical reason.
    fn from_unadmitted_result(
        value: Value,
        request: &EngramInterTurnObservation,
        observing_session: &str,
    ) -> Result<Self, EngramTransportError> {
        if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 16 * 1024) {
            return Err(EngramTransportError::protocol("unadmitted observation receipt exceeds its bound"));
        }
        let receipt: Self = serde_json::from_value(value).map_err(|_| {
            EngramTransportError::protocol("invalid unadmitted observation receipt schema")
        })?;
        receipt.validate_unadmitted(request, observing_session)?;
        Ok(receipt)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn validate_unadmitted(
        &self,
        request: &EngramInterTurnObservation,
        observing_session: &str,
    ) -> Result<(), EngramTransportError> {
        let (reported_change, mut sent): (bool, Vec<&str>) = match &request.occurrence {
            EngramInterTurnOccurrence::UnadmittedTurn { source_change, observed_checks, .. } => (
                source_change.is_some(),
                observed_checks.iter().map(|check| check.host_check_id.as_str()).collect(),
            ),
            EngramInterTurnOccurrence::ObservedCheck { check, .. } => (false, vec![check.host_check_id.as_str()]),
            EngramInterTurnOccurrence::InterTurnChange { .. } => {
                return Err(EngramTransportError::protocol(
                    "an inter-turn change receipt is not an unadmitted observation receipt",
                ));
            }
        };
        let summaries = self.observed_checks.iter()
            .map(|summary| serde_json::from_value::<EngramObservedCheckSummary>(summary.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| EngramTransportError::protocol("invalid observed check summary on the receipt"))?;
        let mut recorded: Vec<&str> = summaries.iter().map(|summary| summary.host_check_id.as_str()).collect();
        sent.sort_unstable();
        recorded.sort_unstable();
        let checks_recorded = sent == recorded
            && summaries.iter().all(|summary| summary.credit == "uncredited");
        let accounted = if reported_change {
            self.is_accounted() || self.is_historical()
        } else {
            (matches!(self.accounting, EngramObservationAccounting::NoSourceChange {})
                && self.opened_obligations.is_empty())
                || self.is_historical()
        };
        if !self.recorded_scope_matches(request, observing_session)? || !checks_recorded || !accounted {
            return Err(EngramTransportError::protocol(
                "unadmitted observation receipt does not record the observed occurrence under its scope and policy",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod unadmitted_observation_tests {
    use super::*;

    fn at(seconds: u32) -> String {
        format!("2026-10-11T00:00:{seconds:02}Z")
    }

    fn built_at() -> chrono::DateTime<chrono::Utc> {
        engram_observation_instant(&at(59), "built_at").unwrap()
    }

    fn source(revision: &str) -> EngramExecutionSourceBasis {
        EngramExecutionSourceBasis {
            workspace_id: "workspace-unadmitted".into(),
            source_revision: revision.into(),
            source_root_generation: Some(2),
            source_root_state: Some(EngramSourceRootState::Named),
        }
    }

    fn basis() -> EngramUnadmittedObservationBasis {
        EngramUnadmittedObservationBasis {
            idempotency_key: "termal-unadmitted:turn-1".into(),
            binding: EngramControlWorkBinding {
                root_execution_id: "root-unadmitted".into(), work_id: "work-unadmitted".into(),
                run_id: "run-unadmitted".into(), claim_id: "claim-unadmitted".into(),
                work_revision: 3, claim_fence: 4,
            },
            root_basis: serde_json::from_value(json!({
                "capture_run_cut": 11, "latest_event": "c".repeat(32),
                "state": { "state": "bound", "workspace_id": "workspace-unadmitted",
                    "generation": 2, "named_at": at(0) },
            })).unwrap(),
            observed_interval: EngramObservationInterval { from: at(10), through: at(50) },
            policy_basis: serde_json::from_value(json!({
                "mode": "account_if_eligible", "project_policy_epoch": 3,
                "policy": "a".repeat(32), "obligation_rule_set": "b".repeat(64),
            })).unwrap(),
        }
    }

    fn change() -> EngramObservedSourceChange {
        EngramObservedSourceChange::ContentComparison {
            workspace_id: "workspace-unadmitted".into(),
            baseline: EngramMeasuredBaseline {
                workspace_id: "workspace-unadmitted".into(),
                source_revision: "content-v1:before".into(),
                observed_at: at(10),
            },
            sighting: EngramMeasuredSighting { source_basis: source("content-v1:after"), observed_at: at(50) },
        }
    }

    fn check(id: &str) -> EngramObservedCheck {
        EngramObservedCheck {
            host_check_id: id.into(),
            check_kind: EngramVerificationKind::Test,
            observed_result: EngramObservedCheckResult::Passed,
            started_at: Some(at(20)),
            finished_at: Some(at(30)),
            observed_at: at(30),
            source_basis: Some(source("content-v1:after")),
            host_evidence_ref: Some("termal-run:abc".into()),
        }
    }

    fn turn(
        source_change: Option<EngramObservedSourceChange>,
        checks: Vec<EngramObservedCheck>,
    ) -> Result<EngramControlRequest, EngramTransportError> {
        engram_unadmitted_turn_request("routing-unadmitted", basis(), "turn-1".into(), source_change, checks, built_at())
    }

    fn observation(request: &EngramControlRequest) -> &EngramInterTurnObservation {
        let EngramControlRequest::ExecutionObserve { observation, .. } = request else { unreachable!() };
        observation
    }

    fn receipt(request: &EngramControlRequest, accounting: Value, checks: Value) -> Value {
        let observation = observation(request);
        json!({
            "decision": "recorded", "observation": "d".repeat(32),
            "position": { "feed": { "kind": "run_execution", "id": "run-unadmitted" }, "position": 12 },
            "observing_session": "producer-session", "binding": observation.binding,
            "admission": "unadmitted", "causality": { "kind": "unknown" },
            "policy_basis": observation.policy_basis, "accounting": accounting,
            "opened_obligations": [], "observed_checks": checks,
            "recorded_at": at(59),
        })
    }

    fn decode(request: &EngramControlRequest, value: Value) -> Result<EngramSourceObservationReceipt, EngramTransportError> {
        EngramSourceObservationReceipt::from_unadmitted_result(value, observation(request), "producer-session")
    }

    #[test]
    fn an_unadmitted_turn_serializes_the_documented_wire_and_replays_identically() {
        let request = turn(Some(change()), vec![check("check-1"), check("check-2")]).unwrap();
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["operation"], "execution_observe");
        assert_eq!(wire["routing_token"], "routing-unadmitted");
        assert_eq!(wire["causality"], json!({ "kind": "unknown" }));
        assert_eq!(wire["observed_interval"], json!({ "from": at(10), "through": at(50) }));
        let occurrence = &wire["occurrence"];
        assert_eq!(occurrence["kind"], "unadmitted_turn");
        assert_eq!(occurrence["host_turn_ref"], "turn-1");
        assert_eq!(occurrence["source_change"]["detection"], "content_comparison");
        assert_eq!(occurrence["observed_checks"][0], json!({
            "host_check_id": "check-1", "check_kind": "test", "observed_result": "passed",
            "started_at": at(20), "finished_at": at(30), "observed_at": at(30),
            "source_basis": serde_json::to_value(source("content-v1:after")).unwrap(),
            "host_evidence_ref": "termal-run:abc",
        }));
        let replay: EngramControlRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(replay).unwrap(), wire, "a retained request replays the same facts");

        let unreported = serde_json::to_value(turn(None, Vec::new()).unwrap()).unwrap();
        assert_eq!(unreported["occurrence"]["source_change"], Value::Null, "no reported change is an explicit null");
        assert_eq!(unreported["occurrence"]["observed_checks"], json!([]));
        let missing = EngramObservedSourceChange::AssumedMissingBaseline {
            workspace_id: "workspace-unadmitted".into(),
            sighting: EngramMeasuredSighting { source_basis: source("content-v1:after"), observed_at: at(50) },
        };
        let assumed = serde_json::to_value(turn(Some(missing), Vec::new()).unwrap()).unwrap();
        assert_eq!(assumed["occurrence"]["source_change"]["detection"], "assumed_missing_baseline");
    }

    #[test]
    fn a_standalone_observed_check_serializes_the_documented_wire() {
        let mut unfinished = check("check-17");
        unfinished.observed_result = EngramObservedCheckResult::Indeterminate;
        unfinished.finished_at = None;
        unfinished.source_basis = None;
        unfinished.host_evidence_ref = None;
        let request = engram_observed_check_request("routing-unadmitted", basis(), "turn-1".into(), unfinished, built_at()).unwrap();
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["occurrence"], json!({
            "kind": "observed_check", "host_turn_ref": "turn-1",
            "check": { "host_check_id": "check-17", "check_kind": "test",
                "observed_result": "indeterminate", "started_at": at(20), "observed_at": at(30) },
        }));
        let replay: EngramControlRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(replay).unwrap(), wire);
    }

    fn with_basis(change: impl FnOnce(&mut EngramUnadmittedObservationBasis)) -> Result<EngramControlRequest, EngramTransportError> {
        let mut changed = basis();
        change(&mut changed);
        engram_unadmitted_turn_request("routing-unadmitted", changed, "turn-1".into(), None, Vec::new(), built_at())
    }

    fn with_change(change: impl FnOnce(&mut EngramMeasuredBaseline, &mut EngramMeasuredSighting)) -> Result<EngramControlRequest, EngramTransportError> {
        let EngramObservedSourceChange::ContentComparison { workspace_id, mut baseline, mut sighting } = change_fixture() else { unreachable!() };
        change(&mut baseline, &mut sighting);
        turn(Some(EngramObservedSourceChange::ContentComparison { workspace_id, baseline, sighting }), Vec::new())
    }

    fn change_fixture() -> EngramObservedSourceChange {
        change()
    }

    #[test]
    fn building_refuses_whole_requests_that_break_a_bound_or_a_relation() {
        // Each case starts from a valid request and breaks one rule; the
        // reason names the rule that fired, so a drifting fixture cannot pass.
        let refused = |result: Result<EngramControlRequest, EngramTransportError>, reason: &str| {
            let error = result.expect_err(reason);
            assert!(error.message.starts_with("unadmitted observation refused: "), "{reason}: {}", error.message);
            assert!(error.message.contains(reason), "expected {reason:?}, got {}", error.message);
        };
        let checks = |count: usize| (0..count).map(|index| check(&format!("check-{index}"))).collect::<Vec<_>>();
        assert!(turn(None, checks(16)).is_ok(), "16 checks fit");
        refused(turn(None, checks(17)), "at most 16 observed checks");
        refused(turn(None, vec![check("same"), check("same")]), "appears twice");

        let mut finished_without_time = check("check-1");
        finished_without_time.finished_at = None;
        refused(turn(None, vec![finished_without_time]), "without finished_at");
        let mut failed_without_time = check("check-1");
        failed_without_time.observed_result = EngramObservedCheckResult::Failed;
        failed_without_time.finished_at = None;
        refused(turn(None, vec![failed_without_time]), "without finished_at");
        let mut outside = check("check-1");
        outside.observed_at = at(55);
        refused(turn(None, vec![outside]), "observed_at lies outside observed_interval");
        let mut backwards = check("check-1");
        backwards.started_at = Some(at(40));
        refused(turn(None, vec![backwards]), "started after it finished");
        let mut long_evidence = check("check-1");
        long_evidence.host_evidence_ref = Some("e".repeat(2_049));
        refused(turn(None, vec![long_evidence]), "host_evidence_ref must be trimmed");
        let mut padded = check("check-1");
        padded.host_check_id = " check-1".into();
        refused(turn(None, vec![padded]), "host_check_id must be trimmed");
        let mut half_root = check("check-1");
        half_root.source_basis.as_mut().unwrap().source_root_state = None;
        refused(turn(None, vec![half_root]), "root generation and state only together");

        refused(
            engram_unadmitted_turn_request("routing-unadmitted", basis(), "t".repeat(129), None, Vec::new(), built_at()),
            "host_turn_ref must be trimmed",
        );
        refused(with_basis(|basis| basis.idempotency_key = "k".repeat(129)), "idempotency_key must be trimmed");
        refused(with_basis(|basis| basis.binding.work_revision = 0), "must be positive");
        refused(with_basis(|basis| basis.binding.claim_fence = 0), "must be positive");
        refused(with_basis(|basis| basis.root_basis.capture_run_cut = 0), "positive capture cut");
        refused(with_basis(|basis| basis.root_basis.latest_event = Some("not-a-record-id".into())), "positive capture cut");
        for (epoch, policy) in [(0, "a".repeat(32)), (-1, "a".repeat(32)), (3, "not-a-record-id".to_owned())] {
            refused(
                with_basis(|basis| basis.policy_basis = EngramObservationPolicyBasis::AccountIfEligible {
                    project_policy_epoch: epoch, policy, obligation_rule_set: "b".repeat(64),
                }),
                "policy_basis needs a positive epoch",
            );
        }
        refused(with_basis(|basis| basis.observed_interval.through = "2026-10-11T00:01:30Z".into()), "after the request was built");
        refused(
            with_basis(|basis| basis.observed_interval = EngramObservationInterval { from: at(50), through: at(10) }),
            "from is after observed_interval.through",
        );

        refused(with_change(|baseline, sighting| baseline.source_revision = sighting.source_basis.source_revision.clone()), "two different revisions");
        refused(with_change(|baseline, _| baseline.workspace_id = "another-workspace".into()), "one workspace throughout");
        refused(with_change(|baseline, sighting| { baseline.observed_at = at(45); sighting.observed_at = at(40); }), "observed after its sighting");
        refused(with_change(|baseline, _| baseline.observed_at = at(5)), "source_change.baseline lies outside");
        refused(with_change(|_, sighting| sighting.observed_at = at(55)), "source_change.sighting lies outside");
        let mut long_revision = source("r".repeat(513).as_str());
        long_revision.source_root_generation = None;
        long_revision.source_root_state = None;
        refused(
            turn(Some(EngramObservedSourceChange::AssumedMissingBaseline {
                workspace_id: "workspace-unadmitted".into(),
                sighting: EngramMeasuredSighting { source_basis: long_revision, observed_at: at(50) },
            }), Vec::new()),
            "source_revision must be trimmed",
        );
        refused(
            engram_unadmitted_turn_request(&"r".repeat(65 * 1_024), basis(), "turn-1".into(), None, Vec::new(), built_at()),
            "byte bound",
        );
    }

    #[test]
    fn an_unadmitted_receipt_names_exactly_the_sent_checks_uncredited_and_matching_accounting() {
        let checks = vec![check("check-1"), check("check-2")];
        let with_change = turn(Some(change()), checks.clone()).unwrap();
        let both = json!([
            { "host_check_id": "check-2", "credit": "uncredited" },
            { "host_check_id": "check-1", "credit": "uncredited" },
        ]);
        let change_accounting = json!({ "kind": "source_change", "source_change": "d".repeat(32) });
        assert!(decode(&with_change, receipt(&with_change, change_accounting.clone(), both.clone())).is_ok());
        assert!(decode(&with_change, receipt(&with_change,
            json!({ "kind": "repeat", "source_change": "f".repeat(32) }), both.clone())).is_ok());
        assert!(decode(&with_change, receipt(&with_change,
            json!({ "kind": "audit_only", "reason": "finished_run" }), both.clone())).is_ok());

        let unreported = turn(None, checks).unwrap();
        let no_change = json!({ "kind": "no_source_change" });
        assert!(decode(&unreported, receipt(&unreported, no_change.clone(), both.clone())).is_ok());
        assert!(decode(&unreported, receipt(&unreported,
            json!({ "kind": "audit_only", "reason": "historical_binding" }), both.clone())).is_ok(),
            "a terminal historical audit also settles no reported change");
        let standalone = engram_observed_check_request("routing-unadmitted", basis(), "turn-1".into(), check("check-3"), built_at()).unwrap();
        assert!(decode(&standalone, receipt(&standalone, no_change.clone(),
            json!([{ "host_check_id": "check-3", "credit": "uncredited" }]))).is_ok());

        let refused = |request: &EngramControlRequest, value: Value, case: &str| {
            assert!(decode(request, value).is_err(), "accepted {case}");
        };
        refused(&with_change, receipt(&with_change, change_accounting.clone(), json!([
            { "host_check_id": "check-1", "credit": "credited" },
            { "host_check_id": "check-2", "credit": "uncredited" },
        ])), "a credited check");
        refused(&with_change, receipt(&with_change, change_accounting.clone(),
            json!([{ "host_check_id": "check-1", "credit": "uncredited" }])), "a missing check");
        refused(&with_change, receipt(&with_change, change_accounting.clone(), json!([
            { "host_check_id": "check-1", "credit": "uncredited" },
            { "host_check_id": "check-2", "credit": "uncredited" },
            { "host_check_id": "check-9", "credit": "uncredited" },
        ])), "an extra check");
        refused(&with_change, receipt(&with_change, change_accounting.clone(), json!([
            { "host_check_id": "check-1", "credit": "uncredited", "verified": true },
            { "host_check_id": "check-2", "credit": "uncredited" },
        ])), "an unknown summary field");
        refused(&with_change, receipt(&with_change,
            json!({ "kind": "source_change", "source_change": "f".repeat(32) }), both.clone()), "a change naming another record");
        refused(&with_change, receipt(&with_change, no_change.clone(), both.clone()), "no change for a reported change");
        refused(&unreported, receipt(&unreported, change_accounting.clone(), both.clone()), "a change for no reported change");
        refused(&unreported, receipt(&unreported,
            json!({ "kind": "audit_only", "reason": "explicit_audit" }), both.clone()), "explicit audit");
        let mut opened = receipt(&unreported, no_change.clone(), both.clone());
        opened["opened_obligations"] = json!(["e".repeat(32)]);
        refused(&unreported, opened, "obligations opened by no change");
        let mut admitted = receipt(&with_change, change_accounting.clone(), both.clone());
        admitted["admission"] = json!("admitted");
        refused(&with_change, admitted, "an admitted record");
        let mut other_run = receipt(&with_change, change_accounting, both);
        other_run["position"]["feed"]["id"] = json!("another-run");
        refused(&with_change, other_run, "another run's feed");
    }
}
