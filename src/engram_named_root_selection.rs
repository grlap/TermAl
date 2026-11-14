// Runtime-only selection of an acknowledged name for the next turn. Naming
// owns validation and acknowledgement; dispatch owns the opening and commit.
// Restore keeps its existing work-binding rule and restores no selection.

#[derive(Clone)]
struct EngramPendingNamingSelection {
    store: EngramAuthorityStoreKey,
    connection: EngramConnectionConfig,
    previous: Option<EngramControlWorkBinding>,
    binding: EngramControlWorkBinding,
    root: EngramNamedRootState,
    named_in_generation: u64,
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
) {
    let root = named_selection_root_locked(inner, store, binding);
    let record = inner
        .session_mut_by_index(index)
        .expect("naming session exists");
    // A later successful explicit name supersedes an earlier pending choice,
    // including naming the running claim again to keep it selected.
    record.engram.pending_naming_selection = Some(EngramPendingNamingSelection {
        store: store.clone(),
        connection: connection.clone(),
        previous: record.engram.work_binding.clone(),
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
