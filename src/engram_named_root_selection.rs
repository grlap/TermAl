// Runtime-only selection of an acknowledged name for the next turn. Naming
// owns validation and acknowledgement; dispatch owns the opening and commit.
// Restore keeps its existing work-binding rule and restores no selection.

#[derive(Clone)]
struct EngramPendingNamingSelection {
    store: EngramAuthorityStoreKey,
    connection: EngramConnectionConfig,
    previous: Option<EngramControlWorkBinding>,
    previous_root: Option<EngramNamedRootState>,
    binding: EngramControlWorkBinding,
    root: EngramNamedRootState,
    named_in_generation: u64,
    applied_in_generation: Option<u64>,
}

// Belongs to one existing closing job, not to the successor's checks. Keeping
// the exact trace and scope also fences an off-thread capture already in flight.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramClosingNamingSuccessor {
    trace: EngramRuntimeObservedTurn,
    scope: EngramRuntimeClosingScope,
    successor: EngramRuntimeClosingScope,
    generation: u64,
}

impl EngramRuntimeClosingJob {
    fn require_current_for_capture(
        &self,
        inner: &StateInner,
        session: &str,
    ) -> Result<(), EngramTransportError> {
        let original = self.scope.require_current(inner, session);
        if original.is_ok() {
            return original;
        }
        let Some(witness) = &self.naming_successor else {
            return original;
        };
        let Some(index) = inner.find_session_index(session) else {
            return original;
        };
        let record = &inner.sessions[index];
        if self.terminal_failure.is_some()
            || witness.trace != self.trace
            || witness.scope != self.scope
            || self.scope.binding.is_none()
            || record.active_turn_generation != witness.generation
            || record.engram.named_root.as_ref() != Some(&witness.successor.root)
            || record.runtime_stop_in_progress
            || record.engram.observation_delete_requested
            || session_resource_reset_in_progress(inner, record)
            || !record
                .engram
                .unadmitted_closing_jobs
                .iter()
                .any(|job| job == self)
        {
            return original;
        }
        let Some(binding) = &self.scope.binding else {
            return original;
        };
        if engram_authority_work_unresolved(inner, &self.scope.store, &binding.work_id) {
            return original;
        }
        let mut current = EngramRuntimeClosingScope::capture(inner, session);
        let mut historical =
            EngramRuntimeClosingScope::capture_with_binding(inner, session, Some(binding));
        if let Ok(scope) = &mut current {
            scope.historical_fields = witness.successor.historical_fields.clone();
        }
        if let Ok(scope) = &mut historical {
            scope.historical_fields = self.scope.historical_fields.clone();
        }
        if current.as_ref() == Ok(&witness.successor) && historical.as_ref() == Ok(&self.scope) {
            Ok(())
        } else {
            original
        }
    }

    fn capture_binding_is_current(
        &self,
        inner: &StateInner,
        session: &str,
        binding: &EngramControlWorkBinding,
    ) -> bool {
        self.scope.binding.as_ref() == Some(binding)
            && self.require_current_for_capture(inner, session).is_ok()
    }
}

fn named_selection_root_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
) -> EngramNamedRootState {
    engram_work_source_root_for_claim(
        &inner.engram_work_source_roots,
        store,
        &binding.work_id,
        &binding.claim_id,
    )
    .map_or(EngramNamedRootState::None, |root| {
        EngramNamedRootState::Bound {
            workspace_id: root.root.clone(),
            generation: root.generation as i64,
            named_at: root.named_at.clone(),
        }
    })
}

fn stage_engram_named_root_selection_locked(
    inner: &mut StateInner,
    index: usize,
    store: &EngramAuthorityStoreKey,
    connection: &EngramConnectionConfig,
    binding: &EngramControlWorkBinding,
) {
    let root = named_selection_root_locked(inner, store, binding);
    let record = inner
        .session_mut_by_index(index)
        .expect("naming session exists");
    // An already-selected claim needs no deferred rewrite. In particular,
    // naming it again cancels an older choice without repairing any root
    // authority that may be lost before the next opening.
    if record.engram.work_binding.as_ref() == Some(binding) {
        record.engram.pending_naming_selection = None;
        return;
    }
    // A later successful explicit name supersedes an earlier pending choice,
    // including naming the running claim again to keep it selected.
    record.engram.pending_naming_selection = Some(EngramPendingNamingSelection {
        store: store.clone(),
        connection: connection.clone(),
        previous: record.engram.work_binding.clone(),
        previous_root: record.engram.named_root.clone(),
        binding: binding.clone(),
        root,
        named_in_generation: record.active_turn_generation,
        applied_in_generation: None,
    });
}

fn apply_engram_named_root_selection_locked(inner: &mut StateInner, index: usize, generation: u64) {
    let record = &inner.sessions[index];
    let Some(selection) = record.engram.pending_naming_selection.clone() else {
        return;
    };
    if selection.applied_in_generation.is_some()
        || generation == selection.named_in_generation
        || record.engram.unadmitted_active_turn.is_some()
    {
        return;
    }
    let current = AppState::engram_source_root_target_locked(inner, &record.session.id)
        .ok()
        .flatten()
        .is_some_and(|target| {
            target.connection == selection.connection
                && target.settings.authority_store_key.as_ref() == Some(&selection.store)
                && target.work_binding == selection.previous
                && record.engram.named_root == selection.previous_root
        })
        && !engram_authority_work_unresolved(inner, &selection.store, &selection.binding.work_id)
        && named_selection_root_locked(inner, &selection.store, &selection.binding)
            == selection.root;
    let record = inner
        .session_mut_by_index(index)
        .expect("opening session exists");
    if !current {
        record.engram.pending_naming_selection = None;
        return;
    }
    // Only the turn during which this acknowledged name was selected can
    // keep its old closing scope across the one successor opening.
    for job in &mut record.engram.unadmitted_closing_jobs {
        if job.terminal_failure.is_none()
            && job.trace.generation == selection.named_in_generation
            && !job.trace.source_attribution_uncertain
            && job.scope.binding.is_some()
            && job.scope.binding == selection.previous
            && Some(&job.scope.root) == selection.previous_root.as_ref()
            && job.scope.store == selection.store
            && job.scope.connection == selection.connection
            && job.trace.opening_root.as_ref().is_some_and(|opening| {
                opening.store == job.scope.store
                    && Some(&opening.binding) == job.scope.binding.as_ref()
                    && matches!((&opening.provenance, &job.scope.root), (
                        EngramRootCapture::Recorded { generation, state: EngramSourceRootState::Named, workspace_id },
                        EngramNamedRootState::Bound { generation: current, workspace_id: root, .. }
                    ) if generation == current && workspace_id == root)
            })
        {
            job.naming_successor = Some(EngramClosingNamingSuccessor {
                trace: job.trace.clone(),
                scope: job.scope.clone(),
                successor: EngramRuntimeClosingScope {
                    binding: Some(selection.binding.clone()),
                    root: selection.root.clone(),
                    ..job.scope.clone()
                },
                generation,
            });
        }
    }
    record.engram.work_binding = Some(selection.binding);
    record.engram.named_root = Some(selection.root);
    record
        .engram
        .pending_naming_selection
        .as_mut()
        .expect("pending selection")
        .applied_in_generation = Some(generation);
}

fn finish_engram_named_root_selection_locked(inner: &mut StateInner, index: usize) {
    let record = &inner.sessions[index];
    if record
        .engram
        .pending_naming_selection
        .as_ref()
        .is_some_and(|selection| selection.applied_in_generation.is_some())
    {
        // Called only after the opening's existing row commit succeeds.
        // This post-commit cleanup changes no persisted or public field and
        // must not replace the committed message delta's mutation stamp.
        inner.sessions[index].engram.pending_naming_selection = None;
    }
}
