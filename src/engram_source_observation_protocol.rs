// Typed observation policy, immutable request facts and accounted producer receipts.
// Record ids are opaque. Both producer record-id formats are admitted without
// treating either one as a source-content fingerprint.
fn engram_observation_object_id(value: &str) -> bool {
    matches!(value.len(), 32 | 64)
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn deserialize_engram_observation_object_id<'de, D>(decoder: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(decoder)?;
    if engram_observation_object_id(&value) {
        Ok(value)
    } else {
        Err(serde::de::Error::custom("expected a lowercase hex record id"))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum EngramObservationPolicyBasis {
    AccountIfEligible {
        project_policy_epoch: i64,
        #[serde(deserialize_with = "deserialize_engram_observation_object_id")]
        policy: String,
        #[serde(deserialize_with = "deserialize_engram_observation_object_id")]
        obligation_rule_set: String,
    },
}

impl EngramDoctorControl {
    fn observation_policy(&self) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        let invalid = || EngramTransportError::protocol(
            "Engram readiness did not supply a coherent v1 observation policy tuple",
        );
        let epoch = self.epoch.filter(|epoch| *epoch > 0).ok_or_else(invalid)?;
        let policy = self.policy.as_ref().filter(|id| engram_observation_object_id(id))
            .ok_or_else(invalid)?;
        let rules = self.obligation_rules.as_ref().filter(|id| engram_observation_object_id(id))
            .ok_or_else(invalid)?;
        if self.schema_version != Some(1) {
            return Err(invalid());
        }
        Ok(EngramObservationPolicyBasis::AccountIfEligible {
            project_policy_epoch: epoch,
            policy: policy.clone(),
            obligation_rule_set: rules.clone(),
        })
    }
}

fn read_engram_observation_policy(
    connection: &EngramConnectionConfig,
    expected_store: &EngramAuthorityStoreKey,
    timeout: Duration,
    host_workdir: &FsPath,
) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
    if timeout.is_zero() {
        return Err(EngramTransportError::deadline("source observation policy budget expired"));
    }
    let receipt = run_engram_readiness_within(
        &connection.binary_path,
        &connection.project_file,
        &connection.home,
        &connection.project_root,
        timeout,
        host_workdir,
    ).map_err(|error| EngramTransportError::transport(error.message))?;
    let store = validate_engram_readiness(
        &receipt, &connection.project_file, &connection.home,
    ).map_err(|error| EngramTransportError::protocol(error.message))?;
    if &store != expected_store {
        return Err(EngramTransportError::local_state(
            "source observation readiness belongs to another store or project",
        ));
    }
    receipt.observation_policy.as_ref().ok_or_else(|| {
        EngramTransportError::protocol("Engram readiness supplied no observation policy tuple")
    })?.observation_policy()
}

// This boundary uses only the producer's explicitly admitted root states;
// unlike display readers it cannot silently retain an unknown state or field.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum EngramObservationRootState {
    None {},
    Bound { workspace_id: String, generation: i64, named_at: String },
    UnboundByRelease { last_generation: i64, released_at_position: i64 },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EngramObservationRootBasis {
    capture_run_cut: i64,
    latest_event: Option<String>,
    state: EngramObservationRootState,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EngramObservationInterval {
    from: String,
    through: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EngramMeasuredBaseline {
    workspace_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    source_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_id: Option<EngramSourceId>,
    observed_at: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EngramMeasuredSighting {
    source_basis: EngramExecutionSourceBasis,
    observed_at: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "detection", rename_all = "snake_case", deny_unknown_fields)]
enum EngramObservedSourceChange {
    ContentComparison {
        workspace_id: String,
        baseline: EngramMeasuredBaseline,
        sighting: EngramMeasuredSighting,
    },
    AssumedMissingBaseline { workspace_id: String, sighting: EngramMeasuredSighting },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramObservedTurnOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EngramInterTurnOccurrence {
    InterTurnChange { source_change: EngramObservedSourceChange },
    SourceSighting { source_basis: EngramExecutionSourceBasis, observed_at: String },
    // Grant-free occurrences of a turn the host observed only after it began
    // (`engram_unadmitted_observation.rs`). A null source change reports no
    // change; it never asserts the source was unchanged. Only that module's
    // tests build them until the runtime-started turn path sends them.
    #[cfg_attr(not(test), allow(dead_code))]
    UnadmittedTurn {
        host_turn_ref: String,
        source_change: Option<EngramObservedSourceChange>,
        observed_checks: Vec<EngramObservedCheck>,
        #[serde(default, rename = "outcome", skip_serializing_if = "Option::is_none")]
        observed_outcome: Option<EngramObservedTurnOutcome>,
    },
    #[cfg_attr(not(test), allow(dead_code))]
    ObservedCheck { host_turn_ref: String, check: EngramObservedCheck },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EngramObservationCausality {
    Unknown {},
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EngramInterTurnObservation {
    idempotency_key: String,
    binding: EngramControlWorkBinding,
    root_basis: EngramObservationRootBasis,
    observed_interval: EngramObservationInterval,
    occurrence: EngramInterTurnOccurrence,
    causality: EngramObservationCausality,
    policy_basis: EngramObservationPolicyBasis,
}

// Measured input only: no provider continuation, session token, grant,
// retry phase or settlement can be reconstructed from these facts.
#[cfg(test)]
#[derive(Clone, Debug)]
struct EngramMeasuredSourceFacts {
    id: String,
    binding: EngramControlWorkBinding,
    baseline: Option<EngramMeasuredSighting>,
    sighting: EngramMeasuredSighting,
    root_basis: Value,
}

#[cfg(test)]
fn validate_engram_measured_source(
    facts: &EngramMeasuredSourceFacts,
    policy_basis: &EngramObservationPolicyBasis,
) -> Result<EngramObservationRootBasis, EngramTransportError> {
    let invalid = |message| EngramTransportError::protocol(message);
    let EngramObservationPolicyBasis::AccountIfEligible {
        project_policy_epoch, policy, obligation_rule_set,
    } = policy_basis;
    if *project_policy_epoch <= 0 || !engram_observation_object_id(policy)
        || !engram_observation_object_id(obligation_rule_set)
        || facts.id.is_empty() || facts.id.len() + "termal-observe:".len() > 128
    {
        return Err(invalid("source observation policy or request identity is invalid"));
    }
    let root_basis: EngramObservationRootBasis = serde_json::from_value(facts.root_basis.clone())
        .map_err(|_| invalid("source observation capture has no admitted canonical root basis"))?;
    validate_engram_measured_source_sighting(&root_basis, &facts.sighting)?;
    Ok(root_basis)
}

fn validate_engram_measured_source_sighting(
    root_basis: &EngramObservationRootBasis,
    sighting: &EngramMeasuredSighting,
) -> Result<(), EngramTransportError> {
    let invalid = |message| EngramTransportError::protocol(message);
    let measured = chrono::DateTime::parse_from_rfc3339(&sighting.observed_at)
        .map_err(|_| invalid("source sighting has no measured timestamp"))?;
    if measured.offset().local_minus_utc() != 0 {
        return Err(invalid("source sighting timestamp is not UTC"));
    }
    let basis = &sighting.source_basis;
    let root_matches = match (&root_basis.state, basis.source_root_generation, basis.source_root_state) {
        (EngramObservationRootState::Bound { workspace_id, generation, named_at },
            Some(measured), Some(EngramSourceRootState::Named)) =>
            measured > 0 && measured == *generation && workspace_id == &basis.workspace_id
                && root_basis.latest_event.is_some()
                && chrono::DateTime::parse_from_rfc3339(named_at).is_ok(),
        (EngramObservationRootState::None {}, None, None) => true,
        (EngramObservationRootState::None {}, Some(generation), Some(EngramSourceRootState::Ended)) =>
            generation > 0 && root_basis.latest_event.is_some(),
        (EngramObservationRootState::UnboundByRelease { last_generation, released_at_position }, None, None) =>
            *last_generation > 0 && *released_at_position > 0,
        _ => false,
    };
    if root_basis.capture_run_cut <= 0
        || root_basis.latest_event.as_ref().is_some_and(|id| !engram_observation_object_id(id))
        || !root_matches
        || !engram_observation_identity_valid(&basis.source_revision, basis.source_id.as_ref())
        || basis.workspace_id.is_empty() || basis.workspace_id.len() > 512
    {
        return Err(invalid("source observation measurements do not match the canonical opening"));
    }
    Ok(())
}

#[cfg(test)]
impl EngramInterTurnObservation {
    fn from_source_change(
        facts: &EngramMeasuredSourceFacts,
        policy_basis: EngramObservationPolicyBasis,
    ) -> Result<Self, EngramTransportError> {
        let root_basis = validate_engram_measured_source(facts, &policy_basis)?;
        let sighting = &facts.sighting;
        let basis = &sighting.source_basis;
        let through = chrono::DateTime::parse_from_rfc3339(&sighting.observed_at)
            .map_err(|_| EngramTransportError::protocol("source sighting has no measured timestamp"))?;
        let (from, source_change) = if let Some(baseline) = &facts.baseline {
            let from = chrono::DateTime::parse_from_rfc3339(&baseline.observed_at)
                .map_err(|_| EngramTransportError::protocol("source baseline has no measured timestamp"))?;
            if baseline.source_basis.workspace_id != basis.workspace_id
                || baseline.source_basis.source_root_generation != basis.source_root_generation
                || baseline.source_basis.source_root_state != basis.source_root_state
                || (baseline.source_basis.source_id == basis.source_id
                    && baseline.source_basis.source_revision == basis.source_revision)
                || baseline.source_basis.source_id.is_some() != basis.source_id.is_some()
                || !engram_observation_identity_valid(&baseline.source_basis.source_revision,
                    baseline.source_basis.source_id.as_ref()) || from > through
            {
                return Err(EngramTransportError::protocol(
                    "source observation has no compatible differing measured baseline"));
            }
            (baseline.observed_at.clone(), EngramObservedSourceChange::ContentComparison {
                workspace_id: basis.workspace_id.clone(),
                baseline: EngramMeasuredBaseline {
                    source_id: baseline.source_basis.source_id.clone(),
                    workspace_id: baseline.source_basis.workspace_id.clone(),
                    source_revision: baseline.source_basis.source_revision.clone(),
                    observed_at: baseline.observed_at.clone(),
                },
                sighting: sighting.clone(),
            })
        } else {
            (sighting.observed_at.clone(), EngramObservedSourceChange::AssumedMissingBaseline {
                workspace_id: basis.workspace_id.clone(), sighting: sighting.clone(),
            })
        };
        Ok(Self {
            idempotency_key: format!("termal-observe:{}", facts.id),
            binding: facts.binding.clone(), root_basis,
            observed_interval: EngramObservationInterval { from, through: sighting.observed_at.clone() },
            occurrence: EngramInterTurnOccurrence::InterTurnChange { source_change },
            causality: EngramObservationCausality::Unknown {}, policy_basis,
        })
    }

    fn from_source_sighting(
        facts: &EngramMeasuredSourceFacts,
        policy_basis: EngramObservationPolicyBasis,
    ) -> Result<Self, EngramTransportError> {
        let root_basis = validate_engram_measured_source(facts, &policy_basis)?;
        if facts.baseline.is_some() {
            return Err(EngramTransportError::protocol("a source sighting has no comparison baseline"));
        }
        Ok(Self {
            idempotency_key: format!("termal-observe:{}", facts.id),
            binding: facts.binding.clone(), root_basis,
            observed_interval: EngramObservationInterval {
                from: facts.sighting.observed_at.clone(),
                through: facts.sighting.observed_at.clone(),
            },
            occurrence: EngramInterTurnOccurrence::SourceSighting {
                source_basis: facts.sighting.source_basis.clone(),
                observed_at: facts.sighting.observed_at.clone(),
            },
            causality: EngramObservationCausality::Unknown {}, policy_basis,
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EngramObservationAccounting {
    Sighting {},
    SourceChange { source_change: Option<String> },
    Repeat { source_change: String },
    NoSourceChange {},
    AuditOnly { reason: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EngramSourceObservationReceipt {
    decision: String,
    #[serde(deserialize_with = "deserialize_engram_observation_object_id")]
    observation: String,
    position: EngramNamedRootReadPosition,
    observing_session: String,
    binding: EngramControlWorkBinding,
    admission: String,
    causality: EngramObservationCausality,
    policy_basis: EngramObservationPolicyBasis,
    accounting: EngramObservationAccounting,
    opened_obligations: Vec<String>,
    observed_checks: Vec<Value>,
    recorded_at: String,
}

impl EngramSourceObservationReceipt {
    fn is_sighting(&self) -> bool {
        matches!(&self.accounting, EngramObservationAccounting::Sighting {})
    }

    #[cfg(test)]
    fn from_sighting_result(
        value: Value, request: &EngramInterTurnObservation, observing_session: &str,
    ) -> Result<Self, EngramTransportError> {
        let receipt = Self::from_recorded_result(value, request, observing_session)?;
        if !matches!(&request.occurrence, EngramInterTurnOccurrence::SourceSighting { .. })
            || !receipt.is_sighting()
        {
            return Err(EngramTransportError::protocol("historical audit does not confirm a source sighting"));
        }
        Ok(receipt)
    }

    fn is_accounted(&self) -> bool {
        match &self.accounting {
            EngramObservationAccounting::SourceChange { source_change } =>
                source_change.as_ref() == Some(&self.observation),
            EngramObservationAccounting::Repeat { source_change } => engram_observation_object_id(source_change),
            _ => false,
        }
    }

    fn from_recorded_result(
        value: Value, request: &EngramInterTurnObservation, observing_session: &str,
    ) -> Result<Self, EngramTransportError> {
        if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > 16 * 1024) {
            return Err(EngramTransportError::protocol("source observation receipt exceeds its bound"));
        }
        let receipt: Self = serde_json::from_value(value).map_err(|_| {
            EngramTransportError::protocol("invalid source observation receipt schema")
        })?;
        receipt.validate_recorded(request, observing_session)?;
        Ok(receipt)
    }

    #[cfg(test)]
    fn from_accounted_result(
        value: Value,
        request: &EngramInterTurnObservation,
        observing_session: &str,
    ) -> Result<Self, EngramTransportError> {
        let receipt = Self::from_recorded_result(value, request, observing_session)?;
        if !receipt.is_accounted() {
            return Err(EngramTransportError::protocol("historical observation does not account for a source change"));
        }
        Ok(receipt)
    }

    // Producer terminal historical reasons resolve recording only. They
    // cannot turn explicit audit, unknown reasons or equality into credit.
    fn is_historical(&self) -> bool {
        matches!(&self.accounting, EngramObservationAccounting::AuditOnly { reason }
            if matches!(reason.as_str(), "root_basis_moved" | "historical_binding" | "finished_run"))
            && self.opened_obligations.is_empty()
    }

    // Whether the receipt records `request` under its exact scope, policy and
    // observer, whatever the occurrence: shared by the inter-turn and the
    // unadmitted receipt checks (`engram_unadmitted_observation.rs`).
    fn recorded_scope_matches(
        &self,
        request: &EngramInterTurnObservation,
        observing_session: &str,
    ) -> Result<bool, EngramTransportError> {
        let recorded_at = chrono::DateTime::parse_from_rfc3339(&self.recorded_at)
            .map_err(|_| EngramTransportError::protocol("source observation receipt has an invalid timestamp"))?;
        let through = chrono::DateTime::parse_from_rfc3339(&request.observed_interval.through)
            .map_err(|_| EngramTransportError::protocol("source observation request has an invalid timestamp"))?;
        Ok(self.decision == "recorded" && self.admission == "unadmitted"
            && self.binding == request.binding && self.causality == request.causality
            && self.policy_basis == request.policy_basis && !observing_session.is_empty()
            && self.observing_session == observing_session
            && self.position.feed.kind == "run_execution"
            && self.position.feed.id == request.binding.run_id && self.position.position > 0
            && recorded_at >= through
            && self.opened_obligations.iter().all(|id| engram_observation_object_id(id)))
    }

    fn validate_recorded(
        &self,
        request: &EngramInterTurnObservation,
        observing_session: &str,
    ) -> Result<(), EngramTransportError> {
        if let EngramInterTurnOccurrence::SourceSighting { source_basis, observed_at } = &request.occurrence {
            validate_engram_measured_source_sighting(&request.root_basis, &EngramMeasuredSighting {
                source_basis: source_basis.clone(), observed_at: observed_at.clone(),
            })?;
            let parse = |value: &str| chrono::DateTime::parse_from_rfc3339(value)
                .map_err(|_| EngramTransportError::protocol("source sighting interval has an invalid timestamp"));
            let from = parse(&request.observed_interval.from)?;
            let through = parse(&request.observed_interval.through)?;
            let measured = parse(observed_at)?;
            if from > measured || measured > through {
                return Err(EngramTransportError::protocol("source sighting lies outside its measured interval"));
            }
        }
        let accounted = match &request.occurrence {
            EngramInterTurnOccurrence::SourceSighting { .. } =>
                self.is_sighting() && self.opened_obligations.is_empty(),
            _ => self.is_accounted(),
        };
        if !self.recorded_scope_matches(request, observing_session)?
            || !self.observed_checks.is_empty() || !(accounted || self.is_historical())
        {
            return Err(EngramTransportError::protocol(
                "source observation receipt does not account for the captured scope and policy",
            ));
        }
        Ok(())
    }
}

impl EngramTransportError {
    #[cfg(test)]
    fn is_definitive_observation_policy_refusal(&self) -> bool {
        self.kind == EngramTransportErrorKind::Remote
            && self.code.as_deref() == Some("execution_observation_policy_basis_mismatch")
    }
}

#[cfg(test)]
mod source_sighting_protocol_tests {
    use super::*;

    fn policy() -> EngramObservationPolicyBasis {
        serde_json::from_value(json!({
            "mode": "account_if_eligible", "project_policy_epoch": 3,
            "policy": "a".repeat(32), "obligation_rule_set": "b".repeat(64),
        })).unwrap()
    }

    fn source_facts() -> EngramMeasuredSourceFacts {
        let basis = EngramExecutionSourceBasis {
            source_id: None,
            workspace_id: "workspace-protocol".into(), source_revision: "content-v1:new".into(),
            source_root_generation: Some(1), source_root_state: Some(EngramSourceRootState::Named),
        };
        EngramMeasuredSourceFacts {
            id: "original-observation".into(),
            binding: EngramControlWorkBinding {
                root_execution_id: "root-protocol".into(), work_id: "work-protocol".into(),
                run_id: "run-protocol".into(), claim_id: "claim-protocol".into(),
                work_revision: 2, claim_fence: 8,
            },
            baseline: Some(EngramMeasuredSighting {
                source_basis: EngramExecutionSourceBasis { source_revision: "content-v1:old".into(), ..basis.clone() },
                observed_at: "2026-10-03T00:00:00Z".into(),
            }),
            sighting: EngramMeasuredSighting { source_basis: basis, observed_at: "2026-10-03T00:01:00Z".into() },
            root_basis: json!({ "capture_run_cut": 9, "latest_event": "c".repeat(32),
                "state": { "state": "bound", "workspace_id": "workspace-protocol",
                    "generation": 1, "named_at": "2026-10-02T23:00:00Z" } }),
        }
    }

    fn receipt(request: &EngramInterTurnObservation) -> Value {
        json!({
            "decision": "recorded", "observation": "d".repeat(32),
            "position": { "feed": { "kind": "run_execution", "id": "run-protocol" }, "position": 10 },
            "observing_session": "producer-session", "binding": request.binding,
            "admission": "unadmitted", "causality": { "kind": "unknown" },
            "policy_basis": request.policy_basis,
            "accounting": { "kind": "source_change", "source_change": "d".repeat(32) },
            "opened_obligations": ["e".repeat(32)], "observed_checks": [],
            "recorded_at": "2026-10-03T00:01:01Z",
        })
    }

    #[test]
    fn source_observation_protocol_preserves_measured_facts_and_missing_baseline() {
        let mut captured = source_facts();
        let request = EngramInterTurnObservation::from_source_change(&captured, policy()).unwrap();
        let wire = serde_json::to_value(EngramControlRequest::ExecutionObserve {
            observation: request.clone(),
        }).unwrap();
        assert_eq!(wire["operation"], "execution_observe");
        assert_eq!(wire["causality"], json!({ "kind": "unknown" }));
        assert_eq!(wire["occurrence"]["source_change"]["detection"], "content_comparison");
        assert_eq!(wire["occurrence"]["source_change"]["baseline"]["observed_at"], "2026-10-03T00:00:00Z");
        assert_eq!(wire["occurrence"]["source_change"]["sighting"]["source_basis"], serde_json::to_value(&captured.sighting.source_basis).unwrap());
        assert_eq!(wire["observed_interval"], json!({ "from": "2026-10-03T00:00:00Z", "through": "2026-10-03T00:01:00Z" }));
        let replay: EngramControlRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(replay).unwrap(), wire, "stored request retains identical facts and identity");
        captured.baseline = None;
        let missing = serde_json::to_value(EngramInterTurnObservation::from_source_change(&captured, policy()).unwrap()).unwrap();
        assert_eq!(missing["occurrence"]["source_change"]["detection"], "assumed_missing_baseline");
        assert_eq!(missing["observed_interval"]["from"], missing["observed_interval"]["through"]);
        assert!(missing["occurrence"]["source_change"].get("baseline").is_none());

        captured.baseline = Some(captured.sighting.clone());
        assert!(EngramInterTurnObservation::from_source_change(&captured, policy()).is_err(), "equal measurements do not invent a change");
        captured.baseline.as_mut().unwrap().source_basis.source_revision = "content-v1:old".into();
        captured.baseline.as_mut().unwrap().source_basis.workspace_id = "unrelated-workspace".into();
        assert!(EngramInterTurnObservation::from_source_change(&captured, policy()).is_err());
        captured.baseline = None;
        captured.root_basis["state"]["extra"] = json!(true);
        assert!(EngramInterTurnObservation::from_source_change(&captured, policy()).is_err());
    }

    #[test]
    fn source_observation_protocol_requires_accounted_exact_scope_and_policy() {
        let request = EngramInterTurnObservation::from_source_change(&source_facts(), policy()).unwrap();
        let valid = receipt(&request);
        assert!(EngramSourceObservationReceipt::from_accounted_result(valid.clone(), &request, "producer-session").is_ok());
        let mut repeat = valid.clone();
        repeat["accounting"] = json!({ "kind": "repeat", "source_change": "f".repeat(64) });
        assert!(EngramSourceObservationReceipt::from_accounted_result(repeat, &request, "producer-session").is_ok());
        for (pointer, wrong) in [
            ("/binding/claim_fence", json!(9)),
            ("/position/feed/id", json!("another-run")),
            ("/observing_session", json!("another-producer-session")),
            ("/policy_basis/project_policy_epoch", json!(4)),
            ("/policy_basis/policy", json!("f".repeat(32))),
            ("/policy_basis/obligation_rule_set", json!("f".repeat(32))),
            ("/recorded_at", json!("2026-10-03T00:00:59Z")),
            ("/recorded_at", json!("not-a-timestamp")),
            ("/admission", json!("admitted")),
            ("/accounting", json!({ "kind": "audit_only", "reason": "root_basis_moved" })),
            ("/accounting", json!({ "kind": "no_source_change" })),
            ("/accounting/source_change", json!("f".repeat(32))),
            ("/observation", json!("invalid-id")),
            ("/observed_checks", json!([{ "host_check_id": "unexpected" }])),
        ] {
            let mut value = valid.clone();
            *value.pointer_mut(pointer).unwrap() = wrong;
            assert!(EngramSourceObservationReceipt::from_accounted_result(value, &request, "producer-session").is_err(), "accepted mismatched receipt field {pointer}");
        }
    }

    #[test]
    fn source_observation_protocol_quiet_sighting_has_no_turn_or_grant_identity() {
        let mut facts = source_facts();
        facts.baseline = None;
        let request = EngramInterTurnObservation::from_source_sighting(&facts, policy()).unwrap();
        let wire = serde_json::to_value(EngramControlRequest::ExecutionObserve {
            observation: request.clone(),
        }).unwrap();
        assert_eq!(wire["occurrence"], json!({
            "kind": "source_sighting", "source_basis": facts.sighting.source_basis,
            "observed_at": facts.sighting.observed_at,
        }));
        assert_eq!(wire["occurrence"]["source_basis"]["source_revision"], "content-v1:new");
        assert!(wire.get("routing_token").is_none());
        assert!(wire["occurrence"].get("host_turn_ref").is_none());
        assert_eq!(request.observed_interval.from, request.observed_interval.through);
        let decoded: EngramControlRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), wire);

        let mut valid = receipt(&request);
        valid["accounting"] = json!({ "kind": "sighting" });
        valid["opened_obligations"] = json!([]);
        let recorded = EngramSourceObservationReceipt::from_sighting_result(
            valid.clone(), &request, "producer-session",
        ).unwrap();
        assert!(recorded.is_sighting());
        assert!(!recorded.is_accounted(), "a quiet sighting invents no source change");
        let mut wrong_time = request.clone();
        if let EngramInterTurnOccurrence::SourceSighting { observed_at, .. } = &mut wrong_time.occurrence {
            *observed_at = "2026-10-03T00:01:02Z".into();
        }
        assert!(EngramSourceObservationReceipt::from_sighting_result(
            valid.clone(), &wrong_time, "producer-session",
        ).is_err(), "a receipt cannot freshen a sighting outside its measured interval");
        let mut wrong_root = request.clone();
        if let EngramObservationRootState::Bound { generation, .. } = &mut wrong_root.root_basis.state {
            *generation += 1;
        }
        assert!(EngramSourceObservationReceipt::from_sighting_result(
            valid.clone(), &wrong_root, "producer-session",
        ).is_err(), "receipt echo does not repair an inconsistent root capture");
        for reason in ["root_basis_moved", "historical_binding", "finished_run"] {
            let mut audit = valid.clone();
            audit["accounting"] = json!({ "kind": "audit_only", "reason": reason });
            let historical = EngramSourceObservationReceipt::from_recorded_result(
                audit.clone(), &request, "producer-session",
            ).unwrap();
            assert!(!historical.is_sighting());
            assert!(!historical.is_accounted());
            assert!(EngramSourceObservationReceipt::from_sighting_result(
                audit, &request, "producer-session",
            ).is_err(), "historical audit never proves freshness");
        }
        let changed = EngramInterTurnObservation::from_source_change(&source_facts(), policy()).unwrap();
        assert!(EngramSourceObservationReceipt::from_recorded_result(
            valid.clone(), &changed, "producer-session",
        ).is_err(), "a sighting cannot account a source-change request");
        for (pointer, wrong) in [
            ("/binding/claim_fence", json!(9)),
            ("/position/feed/id", json!("another-run")),
            ("/observing_session", json!("another-observer")),
            ("/policy_basis/project_policy_epoch", json!(4)),
            ("/policy_basis/policy", json!("f".repeat(32))),
            ("/policy_basis/obligation_rule_set", json!("f".repeat(32))),
            ("/recorded_at", json!("2026-10-03T00:00:59Z")),
            ("/recorded_at", json!("invalid-time")),
            ("/accounting", json!({ "kind": "source_change", "source_change": "d".repeat(32) })),
            ("/accounting", json!({ "kind": "sighting", "source_change": "d".repeat(32) })),
            ("/opened_obligations", json!(["e".repeat(32)])),
            ("/observed_checks", json!([{ "host_check_id": "unexpected" }])),
        ] {
            let mut wrong_receipt = valid.clone();
            *wrong_receipt.pointer_mut(pointer).unwrap() = wrong;
            assert!(EngramSourceObservationReceipt::from_sighting_result(
                wrong_receipt, &request, "producer-session",
            ).is_err(), "accepted mismatched sighting receipt {pointer}");
        }
    }

    #[test]
    fn source_observation_protocol_quiet_sighting_requires_measured_root_policy_and_time() {
        let mut facts = source_facts();
        assert!(EngramInterTurnObservation::from_source_sighting(&facts, policy()).is_err(),
            "quiet reporting cannot silently discard a comparison baseline");
        facts.baseline = None;
        for (pointer, wrong) in [
            ("/capture_run_cut", json!(0)),
            ("/latest_event", json!("invalid-id")),
            ("/state/generation", json!(2)),
            ("/state/workspace_id", json!("another-workspace")),
            ("/state/named_at", json!("invalid-time")),
        ] {
            let mut bad = facts.clone();
            *bad.root_basis.pointer_mut(pointer).unwrap() = wrong;
            assert!(EngramInterTurnObservation::from_source_sighting(&bad, policy()).is_err(),
                "accepted mismatched root field {pointer}");
        }
        for observed_at in ["invalid-time", "2026-10-03T01:01:00+01:00"] {
            let mut bad = facts.clone();
            bad.sighting.observed_at = observed_at.into();
            assert!(EngramInterTurnObservation::from_source_sighting(&bad, policy()).is_err());
        }
        let mut bad = facts.clone();
        bad.sighting.source_basis.source_revision.clear();
        assert!(EngramInterTurnObservation::from_source_sighting(&bad, policy()).is_err());
        let invalid_policy = EngramObservationPolicyBasis::AccountIfEligible {
            project_policy_epoch: 0, policy: "a".repeat(32), obligation_rule_set: "b".repeat(64),
        };
        assert!(EngramInterTurnObservation::from_source_sighting(&facts, invalid_policy).is_err());
        facts.sighting.source_basis.source_root_generation = None;
        facts.sighting.source_basis.source_root_state = None;
        facts.root_basis = json!({ "capture_run_cut": 9, "latest_event": null,
            "state": { "state": "none" } });
        assert!(EngramInterTurnObservation::from_source_sighting(&facts, policy()).is_ok());
    }

    #[test]
    fn source_observation_protocol_work_session_restriction_needs_explicit_current_support() {
        let connection = EngramConnectionConfig {
            binary_path: "fixture-engram".into(), project_file: "fixture/.engram-project".into(),
            home: "fixture/home".into(), project_root: "fixture".into(), actor_id: "host".into(),
            actor_context: None, session_id: "host-session".into(),
        };
        let cache = EngramConnectionCapabilities::default();
        for flag in [None, Some(Value::Null), Some(json!(false)), Some(json!("true")), Some(json!(true))] {
            let mut reply = json!({"session_id":"host-session", "work_session":true,
                "capabilities":["host_work_v1", "verification_record_v1"]});
            if let Some(flag) = &flag {
                reply["verification_evidence_git_tree_applicability_v1"] = flag.clone();
            }
            cache.note_reply(&connection, Some(1), &EngramControlRequest::WorkSessionRead {}, &Ok(reply));
            assert_eq!(cache.work_session_git_tree_applicability(&connection, Some(1)),
                flag.as_ref().and_then(Value::as_bool) == Some(true));
            assert!(!cache.work_session_git_tree_applicability(&connection, Some(2)),
                "a replacement child does not inherit the restricted-evidence flag");
            assert_eq!(cache.work_session_verification_record(&connection, Some(1)),
                EngramVerificationRecordSupport::Supported,
                "missing restricted-evidence support does not change unrestricted record support");
        }
        cache.note_reply(&connection, Some(1), &EngramControlRequest::WorkSessionRead {},
            &Err(EngramTransportError::deadline("lost capability reply")));
        assert!(!cache.work_session_git_tree_applicability(&connection, Some(1)));
    }

    #[test]
    fn source_observation_protocol_requires_one_readiness_tuple_and_definitive_refusal() {
        let control = json!({ "schema_version": 1,
            "epoch": 3, "policy": "a".repeat(32), "obligation_rules": "b".repeat(64) });
        let decoded: EngramDoctorControl = serde_json::from_value(control.clone()).unwrap();
        assert_eq!(decoded.observation_policy().unwrap(), policy(),
            "ordinary accounting uses the coherent tuple without requiring turn admission");
        for field in ["schema_version", "epoch", "policy", "obligation_rules"] {
            let mut incomplete = control.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            let decoded: EngramDoctorControl = serde_json::from_value(incomplete).unwrap();
            assert!(decoded.observation_policy().is_err(), "missing {field} cannot authorize accounting");
        }
        assert!(serde_json::from_value::<EngramObservationPolicyBasis>(json!({ "mode": "audit_only" })).is_err());
        let refusal = EngramTransportError::remote(EngramControlErrorBody {
            code: "execution_observation_policy_basis_mismatch".into(), message: "policy changed".into(),
        });
        assert!(refusal.is_definitive_observation_policy_refusal());
        let mut uncertain = refusal.clone();
        uncertain.kind = EngramTransportErrorKind::Transport;
        assert!(!uncertain.is_definitive_observation_policy_refusal(), "an uncertain transport cannot authorize a new request identity");
        assert!(!EngramTransportError::deadline("lost reply").is_definitive_observation_policy_refusal());
    }

    #[test]
    fn source_observation_protocol_uses_the_same_carrier_without_an_active_named_root() {
        let mut captured = source_facts();
        captured.baseline = None;
        captured.sighting.source_basis.source_root_generation = None;
        captured.sighting.source_basis.source_root_state = None;
        captured.root_basis = json!({ "capture_run_cut": 9, "latest_event": null, "state": { "state": "none" } });
        let request = EngramInterTurnObservation::from_source_change(&captured, policy()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap()["root_basis"]["state"], json!({ "state": "none" }));
        captured.root_basis["latest_event"] = json!("c".repeat(32));
        let after_clear = EngramInterTurnObservation::from_source_change(&captured, policy()).unwrap();
        assert_eq!(after_clear.root_basis.latest_event, Some("c".repeat(32)),
            "unnamed measurements preserve the ended canonical history");
        captured.sighting.source_basis.source_root_generation = Some(1);
        captured.sighting.source_basis.source_root_state = Some(EngramSourceRootState::Ended);
        assert!(EngramInterTurnObservation::from_source_change(&captured, policy()).is_ok());
        captured.sighting.source_basis.source_root_state = Some(EngramSourceRootState::Named);
        assert!(EngramInterTurnObservation::from_source_change(&captured, policy()).is_err(), "a no-root cut cannot claim a live named root");
    }
}
