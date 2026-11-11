// A measurement taken for an evaluation request can compare that turn's
// first close, not admit a turn or certify an evaluation. Consume it at that
// close: an older equality must not bridge a later missing/unmeasured prefix.
// Preserve its original time and binding/root image; ordinary checkpoints
// keep precedence and queued/uncertain observations remain immutable.

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct EngramEvaluationCheckpoint {
    store: EngramAuthorityStoreKey,
    host_turn_ref: String,
    generation: u64,
    checkpoint: EngramRuntimeCheckpointBasis,
}

fn evaluation_closing_checkpoint(
    retained: Option<&EngramEvaluationCheckpoint>,
    trace: &EngramRuntimeObservedTurn,
    scope: &EngramRuntimeClosingScope,
) -> Option<EngramRuntimeCheckpointBasis> {
    let retained = retained?;
    let checkpoint = &retained.checkpoint;
    let EngramNamedRootState::Bound { workspace_id, generation, .. } = &scope.root else {
        return None;
    };
    (retained.store == scope.store
        && retained.host_turn_ref == trace.host_turn_ref
        && retained.generation == trace.generation
        && scope.binding.as_ref() == Some(&checkpoint.binding)
        && &checkpoint.endpoint.workspace_id == workspace_id
        && checkpoint.source_root_generation == Some(*generation)
        && checkpoint.source_root_state == Some(EngramSourceRootState::Named))
        .then(|| checkpoint.clone())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramEvaluationCheckpointScope {
    host_turn_ref: String,
    generation: u64,
    opening: EngramRuntimeOpeningRoot,
}

fn evaluation_checkpoint_scope_locked(
    inner: &StateInner,
    owner: &str,
    place: &EngramBasisPlace,
) -> Option<EngramEvaluationCheckpointScope> {
    let index = inner.find_session_index(owner)?;
    let record = &inner.sessions[index];
    if !ordinary_runtime_observation_enabled(inner, owner)
        || record.runtime_stop_in_progress
        || record.engram.observation_delete_requested
        || session_resource_reset_in_progress(inner, record)
    {
        return None;
    }
    let trace = record.engram.unadmitted_active_turn.as_ref()?;
    if trace.generation != record.active_turn_generation || trace.source_attribution_uncertain {
        return None;
    }
    let opening = trace.opening_root.as_ref()?;
    if runtime_observation_opening_root(inner, owner).as_ref() != Some(opening)
        || !matches!(opening.provenance, EngramRootCapture::Recorded {
            state: EngramSourceRootState::Named, ..
        })
        || opening.source_root.as_ref()?.place() != *place
    {
        return None;
    }
    Some(EngramEvaluationCheckpointScope {
        host_turn_ref: trace.host_turn_ref.clone(),
        generation: trace.generation,
        opening: opening.clone(),
    })
}

impl AppState {
    fn retain_evaluation_checkpoint(
        &self,
        owner: &str,
        place: &EngramBasisPlace,
        scope: &EngramEvaluationCheckpointScope,
        source_id: &EngramSourceId,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if evaluation_checkpoint_scope_locked(&inner, owner, place).as_ref() != Some(scope) {
            return;
        }
        let Some(root) = scope.opening.source_root.as_ref() else {
            return;
        };
        let Some(basis) = scope.opening.provenance.stamp(Some(EngramExecutionSourceBasis {
            workspace_id: root.root.clone(),
            source_revision: String::new(),
            source_id: Some(source_id.clone()),
            source_root_generation: None,
            source_root_state: None,
        })) else {
            return;
        };
        let checkpoint = EngramRuntimeCheckpointBasis::of_sighting(
            scope.opening.binding.clone(),
            &basis,
            &unadmitted_observation_timestamp(),
        );
        let Some(index) = inner.find_session_index(owner) else {
            return;
        };
        inner.sessions[index].engram.evaluation_checkpoint = Some(EngramEvaluationCheckpoint {
            store: scope.opening.store.clone(),
            host_turn_ref: scope.host_turn_ref.clone(),
            generation: scope.generation,
            checkpoint,
        });
        inner.stamp_session_at_index(index);
        // Failure to persist leaves no durable comparison after a restart;
        // it cannot manufacture an acknowledged sighting or test credit.
        if let Err(error) = self.persist_session_internal_locked(&inner, owner) {
            eprintln!("engram> evaluation comparison endpoint was not persisted: {error}");
        }
    }
}
