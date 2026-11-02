// Ordinary queue selection and Resume. Stored authorization history is not
// a dispatch prerequisite, and unknown provider delivery is never replayed.

fn ordinary_queue_entry_is_runnable(record: &SessionRecord, queued: &QueuedPromptRecord) -> bool {
    queued.promotion_disposition_known
        && (queued.promoted_message_index.is_none()
            || promoted_entry_is_proven_unsent(record, queued)
            // Its explicit Resume owns the new write-ahead attempt even
            // after that attempt changes Pending to HandingOff.
            || prompt_delivery_resume_holds(record, &queued.pending_prompt.id))
        && !queued.engram_ordinary_handoff
        && record.prompt_deliveries.get(&queued.pending_prompt.id).is_none_or(|delivery| {
            matches!(delivery.disposition,
                PromptDeliveryDisposition::Pending | PromptDeliveryDisposition::HandingOff)
                && delivery.turn_generation.is_none()
        })
}

// A promoted entry still in the queue is runnable only on positive proof
// that its provider attempt never began, as a host shutdown restores it: a
// Pending receipt with no turn, no started hand-off and no runtime write,
// plus an acknowledged durability hold of exactly this content. Unknown,
// handing-off, ambiguous and receipt-less promoted entries stay held.
fn promoted_entry_is_proven_unsent(record: &SessionRecord, queued: &QueuedPromptRecord) -> bool {
    let prompt_id = &queued.pending_prompt.id;
    record.prompt_deliveries.get(prompt_id).is_some_and(|delivery| {
        delivery.disposition == PromptDeliveryDisposition::Pending
            && delivery.turn_generation.is_none()
            && !delivery.federation_handoff_started
            && delivery.runtime_write == PromptRuntimeWrite::Untracked
    }) && record.queued_prompt_durability_holds.get(prompt_id).is_some_and(|hold| {
        hold.acknowledged
            && hold.failure.is_none()
            && hold.content == queued_prompt_durability_content(queued)
    })
}

fn ordinary_queue_position(record: &SessionRecord) -> Option<usize> {
    record.queued_prompts.iter().position(|queued| ordinary_queue_entry_is_runnable(record, queued))
}

fn queue_has_orphaned_workflow(record: &SessionRecord) -> bool {
    record.queued_prompts.iter()
        .filter(|queued| ordinary_queue_entry_is_runnable(record, queued))
        .find(|queued| queued.source != QueuedPromptSource::Mailbox)
        .is_some_and(|queued| queued.source == QueuedPromptSource::Orchestrator)
}

fn dispatch_queue_position(inner: &StateInner, index: usize) -> Option<usize> {
    ordinary_queue_position(&inner.sessions[index])
}


// Resource resets retain their own exclusion independently of observation.
fn session_resource_reset_in_progress(inner: &StateInner, record: &SessionRecord) -> bool {
    let mut current = record;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current.session.id.as_str()) { return false; }
        if let Some(project_id) = current.session.project_id.as_deref() {
            return inner.engram_project_resets.contains(project_id);
        }
        if !inner.delegation_inventory_complete() { return false; }
        let Some(parent) = inner.delegations.iter()
            .find(|delegation| delegation.child_session_id == current.session.id)
            .and_then(|delegation| inner.find_session_index(&delegation.parent_session_id))
        else { return false; };
        current = &inner.sessions[parent];
    }
}
