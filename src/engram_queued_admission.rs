// Exact ordinary queue ownership across off-lock preparation and WAL acknowledgement.
// This module has no authorization, grant, admission or recovery operation.

#[derive(Clone)]
struct DeferredTurnHandoff(Arc<Mutex<Option<TurnDispatch>>>);

impl std::fmt::Debug for DeferredTurnHandoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeferredTurnHandoff")
    }
}

impl PartialEq for DeferredTurnHandoff {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for DeferredTurnHandoff {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct QueuedTurnOwner {
    prompt_id: String,
    active_turn_generation: u64,
    intent: String,
    stop_generation: u64,
    pause_epoch: u64,
    write_ahead: Option<String>,
    promoted_message_index: Option<usize>,
    capacity_nonce: Option<String>,
    // A continuation carries its original acknowledged receipt. A new nonce
    // for the same prompt is a different attempt, even when its text matches.
    receipt_resume: Option<PromptDeliveryResume>,
}

impl QueuedTurnOwner {
    fn capture(record: &SessionRecord) -> Option<Self> {
        let queued = &record.queued_prompts[ordinary_queue_position(record)?];
        Some(Self {
            prompt_id: queued.pending_prompt.id.clone(),
            active_turn_generation: record.active_turn_generation,
            intent: receipted_prompt_intent(queued),
            stop_generation: record.runtime_stop_generation,
            pause_epoch: record.auto_dispatch_pause_epoch,
            write_ahead: record.prompt_deliveries.get(&queued.pending_prompt.id)
                .map(|delivery| delivery.write_ahead.clone()),
            promoted_message_index: queued.promoted_message_index,
            capacity_nonce: None,
            receipt_resume: None,
        })
    }

    fn capture_promoted(record: &SessionRecord) -> Option<Self> {
        let queued = record.queued_prompts.iter().find(|queued| {
            queued.promoted_message_index.is_some()
                && (record.prompt_deliveries.get(&queued.pending_prompt.id)
                    .is_some_and(|delivery| delivery.turn_generation == Some(record.active_turn_generation)
                        && delivery.disposition == PromptDeliveryDisposition::HandingOff)
                    || record.human_prompt_turn.as_ref().is_some_and(|owner|
                        owner.generation == record.active_turn_generation && owner.prompt_id == queued.pending_prompt.id)
                    || queued.promoted_message_index == record.active_turn_start_message_count)
        })?;
        Some(Self {
            prompt_id: queued.pending_prompt.id.clone(),
            active_turn_generation: record.active_turn_generation,
            intent: receipted_prompt_intent(queued),
            stop_generation: record.runtime_stop_generation,
            pause_epoch: record.auto_dispatch_pause_epoch,
            write_ahead: record.prompt_deliveries.get(&queued.pending_prompt.id)
                .map(|delivery| delivery.write_ahead.clone()),
            promoted_message_index: queued.promoted_message_index,
            capacity_nonce: None,
            receipt_resume: None,
        })
    }

    fn matches(&self, record: &SessionRecord) -> bool {
        record.active_turn_generation == self.active_turn_generation
            && record.runtime_stop_generation == self.stop_generation
            && record.auto_dispatch_pause_epoch == self.pause_epoch
            && record.prompt_deliveries.get(&self.prompt_id)
                .map(|delivery| &delivery.write_ahead) == self.write_ahead.as_ref()
            && self.capacity_nonce.as_ref()
                .is_none_or(|nonce| provider_capacity_resume_owned(record, nonce))
            && self.receipt_resume.as_ref().is_none_or(|resume| {
                record.runtime_stop_generation == resume.stop_generation
                    && record.auto_dispatch_pause_epoch == resume.pause_epoch
                    && record.prompt_deliveries.get(&self.prompt_id).is_some_and(|delivery| {
                        delivery.resume.as_ref() == Some(resume)
                            && prompt_delivery_resume_holds(record, &self.prompt_id)
                            && delivery.write_ahead == resume.write_ahead
                            && delivery.disposition == PromptDeliveryDisposition::HandingOff
                            && delivery.turn_generation.is_none()
                            && delivery.write_ahead_acknowledged
                    })
            })
            && record.queued_prompts.iter().find(|queued| queued.pending_prompt.id == self.prompt_id)
                .is_some_and(|queued| {
                    queued.promoted_message_index == self.promoted_message_index
                        && receipted_prompt_intent(queued) == self.intent
                        && if self.receipt_resume.is_some()
                            || self.promoted_message_index.is_none()
                            || ordinary_queue_entry_is_runnable(record, queued)
                        {
                            ordinary_queue_position(record).is_some_and(|position|
                                record.queued_prompts[position].pending_prompt.id == self.prompt_id)
                        } else {
                            Self::capture_promoted(record).is_some_and(|owner|
                                owner.prompt_id == self.prompt_id && owner.intent == self.intent)
                        }
                })
    }
}
