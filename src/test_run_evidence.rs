// Evidence impact beside the test-run index. New file.
// Owns exact report/receipt associations and their restart-risk projection.
// Does not mint verification, infer credit from process exits or artifacts,
// poll tracker work, or authorize restarting TermAl.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TestRunEvidenceAssociation {
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    source_basis: EngramExecutionSourceBasis,
    producer_observation_id: String,
    action_fingerprint: String,
    reported_outcome: EngramExecutionOutcome,
    #[serde(default)]
    verification_record_id: Option<String>,
    #[serde(default)]
    checkpoint_id: Option<String>,
    #[serde(default)]
    confirmed_cursor: Option<i64>,
    #[serde(default, with = "test_run_shared_lifecycle_read")]
    lifecycle_read: Option<Arc<EngramNamedRootReadResponse>>,
}

// Share a canonical response across its associations without changing its
// persisted object/null representation or requiring serde's global rc feature.
mod test_run_shared_lifecycle_read {
    use super::*;

    pub fn serialize<S: serde::Serializer>(
        value: &Option<Arc<EngramNamedRootReadResponse>>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        value.as_deref().serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Option<Arc<EngramNamedRootReadResponse>>, D::Error> {
        Option::<EngramNamedRootReadResponse>::deserialize(deserializer)
            .map(|value| value.map(Arc::new))
    }
}

fn test_run_lifecycle_binding_matches(
    association: &TestRunEvidenceAssociation,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
) -> bool {
    &association.store == store
        && association.binding.root_execution_id == binding.root_execution_id
        && association.binding.work_id == binding.work_id
        && association.binding.run_id == binding.run_id
        && association.binding.claim_id == binding.claim_id
}

fn test_run_object_id_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl AppState {
    fn associate_reported_test_checks_locked(
        &self,
        inner: &mut StateInner,
        index: usize,
        checks: &[EngramResolvedCheck],
    ) {
        let record = &inner.sessions[index];
        let session_id = record.session.id.clone();
        let Some(binding) = record.engram.work_binding.clone() else {
            return;
        };
        let Some(store) = engram_project_for_session_locked(inner, &session_id)
            .and_then(|project| project.engram.as_ref())
            .and_then(|settings| settings.authority_store_key.clone())
        else {
            return;
        };
        let mut updates = BTreeMap::new();
        for check in checks
            .iter()
            .filter(|check| check.check.command.kind == EngramVerificationKind::Test)
        {
            let producer_observation_id = sha256_hex(
                format!(
                    "termal-turn-check:{}:{}:{}:{}",
                    session_id, check.check.grant_id, check.check.sequence, check.check.key
                )
                .as_bytes(),
            );
            let grant_identity = sha256_hex(check.check.grant_id.as_bytes());
            let tool_identity = sha256_hex(check.check.key.as_bytes());
            let fingerprint = EngramHost::check_fingerprint(&check.check.command);
            let candidates: Vec<String> = inner
                .test_run_observations
                .values()
                .filter(|row| {
                    row.summary.owner_session_id.as_deref() == Some(&session_id)
                        && row.grant_identity.as_deref() == Some(&grant_identity)
                        && row.tool_identity == tool_identity
                        && row.command_fingerprint == fingerprint
                        && row.summary.started_at.as_deref() == Some(&check.check.started_at)
                        && test_run_path_key(&row.summary.worktree)
                            == test_run_path_key(&test_run_display_path(&check.check.target.root))
                })
                .map(|row| row.execution_id.clone())
                .collect();
            // A provider key without an unambiguous original execution cannot
            // establish a correlation. Keep its warning rather than guessing.
            if candidates.len() != 1 {
                continue;
            }
            let Some(row) = inner.test_run_observations.get_mut(&candidates[0]) else {
                continue;
            };
            // The existing declared-artifact adapter owns this verdict. Its
            // failure may override a native exit 0; the UI must retain both
            // that actual judged outcome and the original native exit code.
            if check.check.declared.is_some() {
                row.summary.state = match check.outcome {
                    EngramExecutionOutcome::Succeeded => TestRunState::Passed,
                    EngramExecutionOutcome::Failed => TestRunState::Failed,
                    _ => TestRunState::Unknown,
                };
            }
            if !row.evidence.iter().any(|association| {
                association.producer_observation_id == producer_observation_id
                    && association.store == store
                    && association.binding == binding
                    && association.source_basis == check.basis
            }) {
                row.validated_unbound = false;
                row.final_unbound_proof = None;
                row.evidence.push(TestRunEvidenceAssociation {
                    store: store.clone(),
                    binding: binding.clone(),
                    source_basis: check.basis.clone(),
                    producer_observation_id,
                    reported_outcome: check.outcome,
                    action_fingerprint: fingerprint.clone(),
                    verification_record_id: None,
                    checkpoint_id: None,
                    confirmed_cursor: None,
                    lifecycle_read: None,
                });
            }
            updates.insert(row.execution_id.clone(), row.clone());
        }
        self.persist_test_run_evidence_batch_locked(
            inner,
            updates.into_values().collect(),
            Vec::new(),
        );
    }

    fn settle_test_run_verification_receipt(
        &self,
        report: &EngramTurnReport,
        receipt: &EngramCheckpointReceipt,
        store: &EngramAuthorityStoreKey,
        binding: &EngramControlWorkBinding,
    ) {
        // Engram's transactional TurnCheckpointReceipt returns one verification
        // ObjectId per input in input order. Missing legacy fields, partial
        // lists, repeated IDs or malformed IDs never constitute settlement.
        let Some(checkpoint) = receipt
            .checkpoint
            .as_ref()
            .filter(|id| test_run_object_id_valid(id))
        else {
            return;
        };
        if receipt.verification_evidence.len() != report.verification_evidence.len()
            || receipt._confirmed_cursor < receipt._cursor
            || receipt
                .verification_evidence
                .iter()
                .any(|id| !test_run_object_id_valid(id))
            || receipt
                .verification_evidence
                .iter()
                .collect::<HashSet<_>>()
                .len()
                != receipt.verification_evidence.len()
        {
            return;
        }
        // Resolve the report once before taking StateInner. Keep the first
        // producer and the original input order, including repeated references:
        // an already-settled association may match a later receipt input.
        let mut producers = HashMap::new();
        for producer in &report.observations {
            producers
                .entry(producer.observation_id.as_str())
                .or_insert(producer);
        }
        let mut settlements = HashMap::<_, Vec<_>>::new();
        for (input, record_id) in report
            .verification_evidence
            .iter()
            .zip(&receipt.verification_evidence)
        {
            if input.check_kind != EngramVerificationKind::Test {
                continue;
            }
            let EngramObservationReference::ObservationId { observation_id } =
                &input.producer_observation;
            let Some(producer) = producers.get(observation_id.as_str()) else {
                continue;
            };
            settlements
                .entry(observation_id.as_str())
                .or_default()
                .push((*producer, record_id));
        }
        if settlements.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let mut changed = Vec::new();
        for row in inner.test_run_observations.values_mut() {
            // Most retained history belongs to another store or binding.
            let Some(first) = row.evidence.iter().position(|association| {
                &association.store == store && &association.binding == binding
            }) else {
                continue;
            };
            let mut updated = false;
            for association in &mut row.evidence[first..] {
                if &association.store != store || &association.binding != binding {
                    continue;
                }
                let Some(candidates) =
                    settlements.get(association.producer_observation_id.as_str())
                else {
                    continue;
                };
                for (producer, record_id) in candidates {
                    if association.action_fingerprint != producer.action_fingerprint
                        || association.reported_outcome != producer.outcome
                    {
                        continue;
                    }
                    // A missing source is not a match, even when other fields
                    // happen to agree with a previous report.
                    if producer.source_basis.as_ref() != Some(&association.source_basis) {
                        continue;
                    }
                    if association
                        .verification_record_id
                        .as_ref()
                        .is_some_and(|earlier| earlier != *record_id)
                    {
                        continue;
                    }
                    association.verification_record_id = Some((*record_id).clone());
                    association.checkpoint_id = Some(checkpoint.clone());
                    association.confirmed_cursor = Some(receipt._confirmed_cursor);
                    updated = true;
                    break;
                }
            }
            if updated {
                changed.push(row.clone());
            }
        }
        self.persist_test_run_evidence_batch_locked(&mut inner, changed, Vec::new());
    }

    fn revalidate_test_run_lifecycle(
        &self,
        store: &EngramAuthorityStoreKey,
        binding: &EngramControlWorkBinding,
        read: &EngramNamedRootReadResponse,
    ) {
        if read.validate_authority(store, binding).is_err() {
            return;
        }
        let generation = read.latest_event.as_ref().map(|event| event.generation);
        let read = Arc::new(read.clone());
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let mut changed = Vec::new();
        let mut validated = Vec::new();
        for row in inner.test_run_observations.values_mut() {
            let Some(first) = row.evidence.iter().position(|association| {
                test_run_lifecycle_binding_matches(association, store, binding)
            }) else {
                continue;
            };
            let mut updated = false;
            for association in &mut row.evidence[first..] {
                if !test_run_lifecycle_binding_matches(association, store, binding)
                    || generation != association.source_basis.source_root_generation
                {
                    continue;
                }
                association.lifecycle_read = Some(Arc::clone(&read));
                validated.push((
                    row.execution_id.clone(),
                    association.producer_observation_id.clone(),
                ));
                updated = true;
            }
            if updated {
                changed.push(row.clone());
            }
        }
        inner.test_runs.lifecycle_revalidated.extend(validated);
        let compacted = self.revalidate_test_run_compacted_locked(&mut inner, store, binding, &read);
        self.persist_test_run_evidence_batch_deferring_compaction_locked(&mut inner, changed, compacted);
    }
}

fn test_run_evidence_impact(inner: &StateInner, row: &TestRunCommandObservation) -> Option<bool> {
    if row.evidence.is_empty() {
        return None;
    }
    let mut unknown = false;
    for association in &row.evidence {
        let revalidated = inner.test_runs.lifecycle_revalidated.contains(&(
            row.execution_id.clone(),
            association.producer_observation_id.clone(),
        ));
        if row.monitoring_interrupted_at.is_some()
            && (!revalidated || association.verification_record_id.is_none())
        {
            unknown = true;
            continue;
        }
        if !revalidated && association.lifecycle_read.is_some() {
            // Loaded mutable completion is not a current canonical read.
            // This is independent of historical interruption provenance.
            unknown = true;
            continue;
        }
        if association.verification_record_id.is_none() || association.checkpoint_id.is_none() {
            return Some(true);
        }
        if association.reported_outcome != EngramExecutionOutcome::Succeeded {
            continue;
        }
        // Reuse only the existing canonical lifecycle read. Same-item work in
        // another run, root release, cancellation and evaluation submission
        // cannot masquerade as this exact lifecycle's completed seal.
        let completed = revalidated
            && association.lifecycle_read.as_ref().is_some_and(|read| {
                read.validate_authority(&association.store, &association.binding)
                    .is_ok()
                    && read.run.state == "completed"
                    && read.latest_event.as_ref().map(|event| event.generation)
                        == association.source_basis.source_root_generation
                    && association.source_basis.source_root_generation.is_some()
            });
        if !completed {
            return Some(true);
        }
        // Mutable correlations loaded across a monitoring gap are not served
        // as safe until an existing producer boundary revalidates them.
    }
    if unknown { None } else { Some(false) }
}
