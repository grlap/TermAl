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
    named_turn_ref: Option<String>,
    capture_durable: bool,
    applied_in_generation: Option<u64>,
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
    naming_turn_ref: Option<&str>,
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
        named_turn_ref: naming_turn_ref.map(str::to_owned),
        capture_durable: naming_turn_ref.is_some_and(|turn| record.engram.durable_runtime_captures.contains(turn)),
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

// Only a report durably retained for the exact naming turn releases a host
// controlled successor. Generation numbers are intentionally not identities.
fn engram_named_selection_capture_pending(record: &SessionRecord) -> bool {
    record.engram.pending_naming_selection.as_ref().is_some_and(|selection|
        selection.applied_in_generation.is_none()
            && selection.named_turn_ref.is_some() && !selection.capture_durable)
}

const ENGRAM_ADOPTED_PREDECESSOR_REASON: &str =
    "a successor adopted turn opened before the predecessor capture settled";

fn mark_engram_adopted_naming_predecessor_locked(inner: &mut StateInner, index: usize) {
    let record = &mut inner.sessions[index];
    let Some(selection) = record.engram.pending_naming_selection.as_ref() else { return; };
    if selection.capture_durable || selection.applied_in_generation.is_some() { return; }
    let Some(turn) = selection.named_turn_ref.as_ref() else { return; };
    for job in &mut record.engram.unadmitted_closing_jobs {
        if &job.trace.host_turn_ref == turn && job.terminal_failure.is_none()
            && job.trace.opening_scope.as_ref() == Some(&job.scope)
            && job.scope.binding == selection.previous
        {
            // This is loss of attribution certainty, never capture permission.
            job.trace.source_attribution_uncertain = true;
            job.last_error = Some(ENGRAM_ADOPTED_PREDECESSOR_REASON.to_owned());
            job.scope.historical_fields.insert("adopted_successor_reason".to_owned(), Value::String(ENGRAM_ADOPTED_PREDECESSOR_REASON.to_owned()));
        }
    }
}

impl EngramRuntimeClosingJob {
    fn require_current_for_capture(&self, inner: &StateInner, session: &str) -> Result<(), EngramTransportError> {
        self.scope.require_current(inner, session)
    }
    fn capture_binding_is_current(&self, inner: &StateInner, session: &str, binding: &EngramControlWorkBinding) -> bool {
        self.scope.binding.as_ref() == Some(binding) && self.scope.require_current(inner, session).is_ok()
    }
}
