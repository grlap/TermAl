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
    // Historical checkpoint provenance survives as data. Registered native
    // verification uses its own checked receipt and has no checkpoint.
    #[serde(default)]
    checkpoint_id: Option<String>,
    #[serde(default)]
    confirmed_cursor: Option<i64>,
    #[serde(default, with = "test_run_shared_lifecycle_read")]
    lifecycle_read: Option<Arc<EngramNamedRootReadResponse>>,
}

// Presentation only: equality and settlement use the complete source_basis
// above. Never serialize the owner's identity into automatic run summaries.
fn test_run_source_summary(basis: &EngramExecutionSourceBasis) -> String {
    match &basis.source_id {
        Some(source) => format!("source_id: commit {}, tree {}, {} changes, {} submodules, {} submodule changes",
            source.commit.chars().take(12).collect::<String>(),
            source.tree.chars().take(12).collect::<String>(),
            source.changes.len(), source.submodules.len(),
            source.submodules.iter().map(|child| child.changes.len()).sum::<usize>()),
        None => basis.source_revision.clone(),
    }
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

impl AppState {
    fn settle_test_run_producer_records_locked(
        inner: &mut StateInner,
        settlements: HashMap<&str, Vec<(&EngramExecutionObservationInput, &String)>>,
        store: &EngramAuthorityStoreKey,
        binding: &EngramControlWorkBinding,
    ) -> Vec<TestRunCommandObservation> {
        if settlements.is_empty() {
            return Vec::new();
        }
        // Only rows carrying one of the receipt's producer observations can
        // settle: their union, in key order, replaces the full history scan.
        // Every original predicate below still decides each association.
        let rows: BTreeSet<String> = settlements
            .keys()
            .flat_map(|observation_id| inner.test_run_observations.index().producer(observation_id))
            .cloned()
            .collect();
        #[cfg(test)]
        TEST_TEST_RUN_INDEXED_CANDIDATES.with(|count| count.set(count.get() + rows.len()));
        let mut changed = Vec::new();
        for key in &rows {
            // Settlement fills only association results: no indexed field.
            let settled = inner.test_run_observations.update_unindexed_fields(key, |row| {
                let first = row.evidence.iter().position(|association| {
                    &association.store == store && &association.binding == binding
                })?;
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
                        // A missing source is not a match, even when other
                        // fields happen to agree with a previous report.
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
                        // Native registration writes the record; historical checkpoint
                        // provenance is read-only and cannot be manufactured here.
                        updated = true;
                        break;
                    }
                }
                updated.then(|| row.clone())
            });
            if let Some(Some(row)) = settled {
                changed.push(row);
            }
        }
        changed
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
        // The exact lifecycle's bucket, in key order, replaces the full scan;
        // the original binding and generation predicates still decide.
        let key = TestRunLifecycleKey::new(
            store, &binding.root_execution_id, &binding.work_id, &binding.run_id, &binding.claim_id,
        );
        let rows: Vec<String> = inner.test_run_observations.index().lifecycle(&key).cloned().collect();
        #[cfg(test)]
        TEST_TEST_RUN_INDEXED_CANDIDATES.with(|count| count.set(count.get() + rows.len()));
        let mut changed = Vec::new();
        let mut validated = Vec::new();
        for row_key in &rows {
            // Revalidation fills only lifecycle reads: no indexed field.
            let revalidated = inner.test_run_observations.update_unindexed_fields(row_key, |row| {
                let first = row.evidence.iter().position(|association| {
                    test_run_lifecycle_binding_matches(association, store, binding)
                })?;
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
                updated.then(|| row.clone())
            });
            if let Some(Some(row)) = revalidated {
                changed.push(row);
            }
        }
        inner.test_runs.lifecycle_revalidated.extend(validated);
        let compacted = self.revalidate_test_run_compacted_locked(&mut inner, store, binding, &read);
        self.persist_test_run_evidence_batch_deferring_compaction_locked(&mut inner, changed, compacted);
    }
}

fn test_run_evidence_impact(inner: &impl TestRunProjectionSource, row: &TestRunCommandObservation) -> Option<bool> {
    if row.evidence.is_empty() {
        return None;
    }
    let mut unknown = false;
    for association in &row.evidence {
        let revalidated = inner.projection_index().lifecycle_revalidated.contains(&(
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
        if association.verification_record_id.is_none() {
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
