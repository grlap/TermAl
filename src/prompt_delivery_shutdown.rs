// Returns a carrier still owned by the caller to a dormant queue at host
// shutdown. Durable unknown effect evidence never becomes replay permission.
impl AppState {
    fn hold_unsent_dispatch_for_shutdown(&self, dispatch: &TurnDispatch) {
        let identity = dispatch.federation_handoff();
        if identity.is_some_and(|identity| identity.server_instance_id != self.server_instance_id) {
            return;
        }
        // Same order as executor evidence: writer, connection, state. The
        // writer remains held so a concurrent EffectAttempt cannot invalidate
        // the not-started proof while the exact receipt is returned to pending.
        let _writer = if identity.is_some() {
            match self.mailbox_store.lock_writer("retain unsent prompt at shutdown") {
                Ok(writer) => Some(writer),
                Err(error) => {
                    eprintln!("shutdown prompt retention deferred: {error:#}");
                    return;
                }
            }
        } else { None };
        let unknown = if let Some(identity) = identity {
            let row = self.mailbox_store.connection().and_then(|connection|
                federation_executor_read(&connection, &identity.operation.central_host_id,
                    &identity.operation.operation_key));
            match row {
                Ok(Some(row)) if row.operation == identity.operation => match row.effect_state.as_str() {
                    "not_started" => false,
                    "unknown" => true,
                    _ => return, // Positive runtime evidence is not an unsent carrier.
                },
                Ok(_) => return,
                Err(error) => {
                    eprintln!("shutdown prompt evidence unavailable: {error:#}");
                    return;
                }
            }
        } else { false };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(dispatch.session_id()) else { return; };
        let record = &inner.sessions[index];
        let turn = dispatch.active_turn_generation();
        if !record.runtime.matches_runtime_token(dispatch.runtime_token())
            || record.active_turn_generation != turn
            || record.runtime_stop_in_progress
        { return; }
        let prompt = record.prompt_deliveries.iter().filter(|(prompt, delivery)|
            delivery.disposition == PromptDeliveryDisposition::HandingOff
                && delivery.turn_generation == Some(turn)
                && !delivery.federation_handoff_started
                && delivery.runtime_write != PromptRuntimeWrite::Begun
                && match identity {
                    Some(identity) => prompt.as_str() == identity.prompt_id
                        && delivery.federation_operation.as_ref() == Some(&identity.operation)
                        && delivery.write_ahead == identity.write_ahead,
                    None => delivery.federation_operation.is_none(),
                })
            .max_by_key(|(_, delivery)| delivery.sequence)
            .map(|(prompt, _)| prompt.clone());
        let Some(prompt) = prompt else { return; };
        let mut entry = match record.prompt_deliveries[&prompt].entry.clone() {
            Some(entry) if entry.pending_prompt.id == prompt => entry,
            _ => return,
        };
        if record.queued_prompts.iter().any(|queued| queued.pending_prompt.id == prompt
            && queued_prompt_durability_content(queued) != queued_prompt_durability_content(&entry))
        { return; }
        if entry.promoted_message_index.is_none() {
            entry.promoted_message_index = record.prompt_deliveries[&prompt].message_id.as_deref()
                .and_then(|id| cached_message_index_on_record(record, id))
                .map(|local| global_message_index(record, local));
        }
        let record = inner.session_mut_by_index(index).expect("session index checked");
        let delivery = record.prompt_deliveries.get_mut(&prompt).expect("receipt checked");
        if unknown {
            // No provider send follows, but a previously recorded uncertainty
            // stays held. It is never rewritten to not_started or re-enqueued.
            delivery.disposition = PromptDeliveryDisposition::Ambiguous;
            delivery.resume = None;
            delivery.reason = Some("host shutdown held the carrier; retained effect evidence is unknown".into());
            retire_queued_prompt(record, &prompt);
        } else {
            delivery.disposition = PromptDeliveryDisposition::Pending;
            delivery.turn_generation = None;
            delivery.write_ahead = Uuid::new_v4().to_string();
            delivery.federation_handoff_started = false;
            delivery.write_ahead_acknowledged = false;
            delivery.runtime_write = PromptRuntimeWrite::Untracked;
            delivery.resume = None;
            delivery.entry = None;
            delivery.reason = Some("host shutdown before runtime delivery; the prompt was not sent".into());
            record.queued_prompts.retain(|queued| queued.pending_prompt.id != prompt);
            // Promotion pruned the old hold. The restored entry includes its
            // transcript position, so only a fresh proof of this content may
            // release it. Loading the committed row supplies that proof.
            // Restored local receipts receive the same standalone protection.
            record.queued_prompt_durability_holds.insert(prompt.clone(), QueuedPromptDurabilityHold {
                nonce: Uuid::new_v4().to_string(),
                content: queued_prompt_durability_content(&entry),
                failure: None,
                acknowledged: false,
            });
            record.queued_prompts.push_front(entry.clone());
            if entry.has_engram_intent() {
                detach_engram_pending_dispatch_keeping_uncertain_begin(record);
            } else {
                take_and_abandon_engram_pending_dispatch(record);
            }
        }
        if record.session.status == SessionStatus::Active { record.session.status = SessionStatus::Idle; }
        record.file_change_tracking = None;
        record.active_turn_mailbox_notification = None;
        sync_pending_prompts(record);
        sync_prompt_delivery_view(record);
        if let Err(error) = self.commit_locked(&mut inner) {
            eprintln!("shutdown prompt retention persistence failed: {error:#}");
        }
    }
}
