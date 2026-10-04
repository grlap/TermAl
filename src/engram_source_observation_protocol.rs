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
        if self.schema_version != Some(1) || self.required_assurance != "turn_gated" {
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
    ).map_err(|error| EngramTransportError::transport(error.message))?;
    let store = validate_engram_readiness(
        &receipt, &connection.project_file, &connection.home, true,
    ).map_err(|error| EngramTransportError::protocol(error.message))?;
    if &store != expected_store {
        return Err(EngramTransportError::local_state(
            "source observation readiness belongs to another store or project",
        ));
    }
    receipt.control.observation_policy()
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
    source_revision: String,
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EngramInterTurnOccurrence {
    InterTurnChange { source_change: EngramObservedSourceChange },
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

impl EngramInterTurnObservation {
    fn from_intent(
        intent: &EngramSourceObservationIntent,
        policy_basis: EngramObservationPolicyBasis,
    ) -> Result<Self, EngramTransportError> {
        let invalid = |message| EngramTransportError::protocol(message);
        let EngramObservationPolicyBasis::AccountIfEligible {
            project_policy_epoch, policy, obligation_rule_set,
        } = &policy_basis;
        if *project_policy_epoch <= 0 || !engram_observation_object_id(policy)
            || !engram_observation_object_id(obligation_rule_set)
            || intent.id.is_empty() || intent.id.len() + "termal-observe:".len() > 128
        {
            return Err(invalid("source observation policy or request identity is invalid"));
        }
        let root_basis: EngramObservationRootBasis = serde_json::from_value(intent.root_basis.clone())
            .map_err(|_| invalid("source observation capture has no admitted canonical root basis"))?;
        let sighting = &intent.sighting;
        let through = chrono::DateTime::parse_from_rfc3339(&sighting.observed_at)
            .map_err(|_| invalid("source sighting has no measured timestamp"))?.with_timezone(&chrono::Utc);
        let workspace_id = &sighting.basis.workspace_id;
        let root_matches = match (&root_basis.state, sighting.basis.source_root_generation, sighting.basis.source_root_state) {
            (EngramObservationRootState::Bound { workspace_id: named_workspace, generation, named_at }, Some(measured), Some(EngramSourceRootState::Named)) =>
                measured > 0 && measured == *generation && named_workspace == workspace_id
                    && root_basis.latest_event.is_some()
                    && chrono::DateTime::parse_from_rfc3339(named_at).is_ok(),
            // A normal clear retains its Ended event in the canonical proof.
            // An unnamed workdir measurement has no named generation/state.
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
            || sighting.basis.source_revision.trim().is_empty()
            || sighting.basis.source_revision.len() > 512
            || workspace_id.is_empty() || workspace_id.len() > 512
        {
            return Err(invalid("source observation measurements do not match the canonical opening"));
        }
        let (from, source_change) = if let Some(baseline) = &intent.baseline {
            let from = chrono::DateTime::parse_from_rfc3339(&baseline.observed_at)
                .map_err(|_| invalid("source baseline has no measured timestamp"))?.with_timezone(&chrono::Utc);
            if baseline.basis.workspace_id != sighting.basis.workspace_id
                || baseline.basis.source_root_generation != sighting.basis.source_root_generation
                || baseline.basis.source_root_state != sighting.basis.source_root_state
                || baseline.basis.source_revision == sighting.basis.source_revision
                || baseline.basis.source_revision.trim().is_empty()
                || baseline.basis.source_revision.len() > 512 || from > through
            {
                return Err(invalid("source observation has no compatible differing measured baseline"));
            }
            (baseline.observed_at.clone(), EngramObservedSourceChange::ContentComparison {
                workspace_id: workspace_id.clone(),
                baseline: EngramMeasuredBaseline {
                    workspace_id: baseline.basis.workspace_id.clone(),
                    source_revision: baseline.basis.source_revision.clone(), observed_at: baseline.observed_at.clone(),
                },
                sighting: EngramMeasuredSighting { source_basis: sighting.basis.clone(), observed_at: sighting.observed_at.clone() },
            })
        } else {
            (sighting.observed_at.clone(), EngramObservedSourceChange::AssumedMissingBaseline {
                workspace_id: workspace_id.clone(),
                sighting: EngramMeasuredSighting { source_basis: sighting.basis.clone(), observed_at: sighting.observed_at.clone() },
            })
        };
        Ok(Self {
            idempotency_key: format!("termal-observe:{}", intent.id),
            binding: intent.binding.clone(), root_basis,
            observed_interval: EngramObservationInterval { from, through: sighting.observed_at.clone() },
            occurrence: EngramInterTurnOccurrence::InterTurnChange { source_change },
            causality: EngramObservationCausality::Unknown {}, policy_basis,
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EngramObservationAccounting {
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
    fn is_accounted(&self) -> bool {
        match &self.accounting {
            EngramObservationAccounting::SourceChange { source_change } =>
                source_change.as_ref() == Some(&self.observation),
            EngramObservationAccounting::Repeat { source_change } => engram_observation_object_id(source_change),
            _ => false,
        }
    }

    fn is_root_move(&self) -> bool {
        matches!(&self.accounting, EngramObservationAccounting::AuditOnly { reason }
            if reason == "root_basis_moved")
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

    fn validate_recorded(
        &self,
        request: &EngramInterTurnObservation,
        observing_session: &str,
    ) -> Result<(), EngramTransportError> {
        let recorded_at = chrono::DateTime::parse_from_rfc3339(&self.recorded_at)
            .map_err(|_| EngramTransportError::protocol("source observation receipt has an invalid timestamp"))?;
        let through = chrono::DateTime::parse_from_rfc3339(&request.observed_interval.through)
            .map_err(|_| EngramTransportError::protocol("source observation request has an invalid timestamp"))?;
        // Producer terminal historical reasons resolve recording only. They
        // cannot turn explicit audit, unknown reasons or equality into credit.
        let historical = matches!(&self.accounting, EngramObservationAccounting::AuditOnly { reason }
            if matches!(reason.as_str(), "root_basis_moved" | "historical_binding" | "finished_run"))
            && self.opened_obligations.is_empty();
        if self.decision != "recorded" || self.admission != "unadmitted"
            || self.binding != request.binding || self.causality != request.causality
            || self.policy_basis != request.policy_basis || observing_session.is_empty()
            || self.observing_session != observing_session
            || self.position.feed.kind != "run_execution"
            || self.position.feed.id != request.binding.run_id || self.position.position <= 0
            || recorded_at < through
            || !self.opened_obligations.iter().all(|id| engram_observation_object_id(id))
            || !self.observed_checks.is_empty() || !(self.is_accounted() || historical)
        {
            return Err(EngramTransportError::protocol(
                "source observation receipt does not account for the captured scope and policy",
            ));
        }
        Ok(())
    }
}

impl EngramTransportError {
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

    fn intent() -> EngramSourceObservationIntent {
        let basis = EngramExecutionSourceBasis {
            workspace_id: "workspace-protocol".into(), source_revision: "content-v1:new".into(),
            source_root_generation: Some(1), source_root_state: Some(EngramSourceRootState::Named),
        };
        EngramSourceObservationIntent {
            id: "original-observation".into(), session_id: "host-session".into(),
            prompt_id: "original-prompt".into(), dispatch_generation: 42,
            active_turn_generation: 7, grant_id: "original-grant".into(),
            binding: EngramControlWorkBinding {
                root_execution_id: "root-protocol".into(), work_id: "work-protocol".into(),
                run_id: "run-protocol".into(), claim_id: "claim-protocol".into(),
                work_revision: 2, claim_fence: 8,
            },
            connection: EngramConnectionConfig {
                binary_path: PathBuf::from("protocol/engram"), project_file: PathBuf::from("protocol/.engram-project"),
                home: PathBuf::from("protocol/home"), project_root: PathBuf::from("protocol"),
                actor_id: "protocol-actor".into(), actor_context: None, session_id: "host-session".into(),
            },
            routing_token: "original-routing".into(), observing_session: Some("producer-session".into()),
            call_timeout_ms: ENGRAM_DEFAULT_CALL_TIMEOUT_MS,
            baseline: Some(EngramSourceSighting {
                basis: EngramExecutionSourceBasis { source_revision: "content-v1:old".into(), ..basis.clone() },
                observed_at: "2026-10-03T00:00:00Z".into(),
            }),
            sighting: EngramSourceSighting { basis, observed_at: "2026-10-03T00:01:00Z".into() },
            root_basis: json!({ "capture_run_cut": 9, "latest_event": "c".repeat(32),
                "state": { "state": "bound", "workspace_id": "workspace-protocol",
                    "generation": 1, "named_at": "2026-10-02T23:00:00Z" } }),
            phase: EngramSourceObservationPhase::Captured, continuation_released: false,
            finalization_complete: false,
            delivery_retired: false, grant_settlement: None, grant_settlement_request: None,
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
        let mut captured = intent();
        let request = EngramInterTurnObservation::from_intent(&captured, policy()).unwrap();
        let wire = serde_json::to_value(EngramControlRequest::ExecutionObserve {
            routing_token: "original-routing".into(), observation: request.clone(),
        }).unwrap();
        assert_eq!(wire["operation"], "execution_observe");
        assert_eq!(wire["causality"], json!({ "kind": "unknown" }));
        assert_eq!(wire["occurrence"]["source_change"]["detection"], "content_comparison");
        assert_eq!(wire["occurrence"]["source_change"]["baseline"]["observed_at"], "2026-10-03T00:00:00Z");
        assert_eq!(wire["occurrence"]["source_change"]["sighting"]["source_basis"], serde_json::to_value(&captured.sighting.basis).unwrap());
        assert_eq!(wire["observed_interval"], json!({ "from": "2026-10-03T00:00:00Z", "through": "2026-10-03T00:01:00Z" }));
        let replay: EngramControlRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(replay).unwrap(), wire, "persisted request replays identical facts and identity");
        captured.baseline = None;
        let missing = serde_json::to_value(EngramInterTurnObservation::from_intent(&captured, policy()).unwrap()).unwrap();
        assert_eq!(missing["occurrence"]["source_change"]["detection"], "assumed_missing_baseline");
        assert_eq!(missing["observed_interval"]["from"], missing["observed_interval"]["through"]);
        assert!(missing["occurrence"]["source_change"].get("baseline").is_none());

        captured.baseline = Some(captured.sighting.clone());
        assert!(EngramInterTurnObservation::from_intent(&captured, policy()).is_err(), "equal measurements do not invent a change");
        captured.baseline.as_mut().unwrap().basis.source_revision = "content-v1:old".into();
        captured.baseline.as_mut().unwrap().basis.workspace_id = "unrelated-workspace".into();
        assert!(EngramInterTurnObservation::from_intent(&captured, policy()).is_err());
        captured.baseline = None;
        captured.root_basis["state"]["extra"] = json!(true);
        assert!(EngramInterTurnObservation::from_intent(&captured, policy()).is_err());
    }

    #[test]
    fn source_observation_protocol_requires_accounted_exact_scope_and_policy() {
        let request = EngramInterTurnObservation::from_intent(&intent(), policy()).unwrap();
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
    fn source_observation_protocol_requires_one_readiness_tuple_and_definitive_refusal() {
        let control = json!({ "schema_version": 1, "required_assurance": "turn_gated",
            "epoch": 3, "policy": "a".repeat(32), "obligation_rules": "b".repeat(64) });
        let decoded: EngramDoctorControl = serde_json::from_value(control.clone()).unwrap();
        assert_eq!(decoded.observation_policy().unwrap(), policy());
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
        let mut captured = intent();
        captured.baseline = None;
        captured.sighting.basis.source_root_generation = None;
        captured.sighting.basis.source_root_state = None;
        captured.root_basis = json!({ "capture_run_cut": 9, "latest_event": null, "state": { "state": "none" } });
        let request = EngramInterTurnObservation::from_intent(&captured, policy()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap()["root_basis"]["state"], json!({ "state": "none" }));
        captured.root_basis["latest_event"] = json!("c".repeat(32));
        let after_clear = EngramInterTurnObservation::from_intent(&captured, policy()).unwrap();
        assert_eq!(after_clear.root_basis.latest_event, Some("c".repeat(32)),
            "unnamed measurements preserve the ended canonical history");
        captured.sighting.basis.source_root_generation = Some(1);
        captured.sighting.basis.source_root_state = Some(EngramSourceRootState::Ended);
        assert!(EngramInterTurnObservation::from_intent(&captured, policy()).is_ok());
        captured.sighting.basis.source_root_state = Some(EngramSourceRootState::Named);
        assert!(EngramInterTurnObservation::from_intent(&captured, policy()).is_err(), "a no-root cut cannot claim a live named root");
    }
}
