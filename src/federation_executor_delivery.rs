// Owns received-operation -> exact native queue association across two stores.
// A durable inbox owns recovery even when queue persistence cannot yet settle.

#[derive(Clone)]
struct FederationExecutorLiveOwner {
    queue: EngramQueuedAdmissionOwner,
    operation: FederationExecutorOperation,
    stop_generation: u64,
    pause_epoch: u64,
    signal: Arc<FederationExecutorWakeSignal>,
}

impl FederationExecutorLiveOwner {
    fn matches(&self, record: &SessionRecord) -> bool {
        !self.signal.is_stopping() && self.queue.matches(record) && record.runtime_stop_generation == self.stop_generation
            && record.auto_dispatch_pause_epoch == self.pause_epoch
            && !record.orchestrator_auto_dispatch_blocked && !record.runtime_stop_in_progress
            && record.prompt_deliveries.get(&self.operation.queue_identity()).is_some_and(|receipt|
                receipt.federation_operation.as_ref() == Some(&self.operation))
    }
}

fn federation_executor_association_retired(record: &SessionRecord, operation: &FederationExecutorOperation) -> bool {
    let prompt = operation.queue_identity();
    record.prompt_deliveries.get(&prompt).is_some_and(|receipt|
        receipt.federation_operation.as_ref() == Some(operation)
            && (receipt.disposition != PromptDeliveryDisposition::Pending
                || !record.queued_prompts.iter().any(|entry| entry.pending_prompt.id == prompt)))
}

impl AppState {
    fn federation_executor_live_key(operation: &FederationExecutorOperation) -> (String, String, String) {
        (operation.central_host_id.clone(), operation.operation_key.clone(), operation.session_id.clone())
    }

    fn retain_federation_executor_live_activation(&self, operation: &FederationExecutorOperation) -> Result<(), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        if federation_executor_operation_retired(&inner, operation) {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        let index = inner.find_session_index(&operation.session_id).expect("binding checked");
        let record = &inner.sessions[index];
        let mut live = self.federation_executor_wake_signal.live.lock().expect("live activation mutex poisoned");
        let key = Self::federation_executor_live_key(operation);
        if record.orchestrator_auto_dispatch_blocked || record.runtime_stop_in_progress {
            live.remove(&key);
        } else {
            live.entry(key).or_insert_with(|| FederationExecutorLiveActivation {
                server_instance_id: self.server_instance_id.clone(),
                stop_generation: record.runtime_stop_generation, pause_epoch: record.auto_dispatch_pause_epoch,
            });
        }
        Ok(())
    }

    fn dispatch_federation_executor_live_activation(&self, operation: &FederationExecutorOperation) {
        if self.federation_executor_wake_signal.is_stopping() { return; }
        let prompt = operation.queue_identity();
        let owner = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let mut live = self.federation_executor_wake_signal.live.lock().expect("live activation mutex poisoned");
            let key = Self::federation_executor_live_key(operation);
            let Some(intent) = live.get(&key) else { return; };
            if federation_executor_operation_retired(&inner, operation) { live.remove(&key); return; }
            let record = &inner.sessions[inner.find_session_index(&operation.session_id).expect("binding checked")];
            if intent.server_instance_id != self.server_instance_id
                || intent.stop_generation != record.runtime_stop_generation
                || intent.pause_epoch != record.auto_dispatch_pause_epoch
                || record.orchestrator_auto_dispatch_blocked || record.runtime_stop_in_progress
            { live.remove(&key); return; }
            // An unsettled durability/settings/quarantine hold is retryable.
            if federation_executor_target_locked(&inner, operation).is_err() { return; }
            let current = intent.server_instance_id == self.server_instance_id
                && intent.stop_generation == record.runtime_stop_generation
                && intent.pause_epoch == record.auto_dispatch_pause_epoch
                && !record.orchestrator_auto_dispatch_blocked && !record.runtime_stop_in_progress
                && record.prompt_deliveries.get(&prompt).is_some_and(|receipt|
                    receipt.disposition == PromptDeliveryDisposition::Pending
                        && receipt.federation_operation.as_ref() == Some(operation));
            if !current { live.remove(&key); return; }
            let Some(queue) = EngramQueuedAdmissionOwner::capture(record).filter(|owner| owner.prompt_id == prompt)
            else { return; };
            let owner = FederationExecutorLiveOwner { queue, operation: operation.clone(),
                stop_generation: intent.stop_generation, pause_epoch: intent.pause_epoch,
                signal: self.federation_executor_wake_signal.clone() };
            live.remove(&key);
            owner
        };
        self.revalidate_queued_mailbox_wakeups_before_dispatch(&operation.session_id);
        match self.start_next_queued_turn_off_lock_for_owner(&operation.session_id, false, false,
            Some(QueuedDrainOwner::FederationWake(owner)))
        {
            Ok(Some(started)) => { let _ = deliver_turn_dispatch(self, started.dispatch); }
            Ok(None) => {}
            Err(error) => eprintln!("remote wake activation remains queued: {error:#}"),
        }
    }

    fn prove_federation_queue_content(&self, target: PersistFenceTarget) -> Result<(), ApiError> {
        // The synchronous test/fallback writer may already have committed it.
        let connection = rusqlite::Connection::open(self.persistence_path.as_path())
            .map_err(|_| federation_transport_unavailable())?;
        if target.is_already_durable(&connection).map_err(|_| federation_transport_unavailable())? {
            return Ok(());
        }
        drop(connection);
        let deadline = std::time::Instant::now() + QUEUED_PROMPT_DURABILITY_BUDGET;
        let (fence, waiter) = PersistFence::new(target, deadline);
        self.persist_tx.send(PersistRequest::Fence(Box::new(fence)))
            .map_err(|_| federation_transport_unavailable())?;
        waiter.wait_until(deadline).ok_or_else(federation_transport_unavailable)?
            .map_err(|_| federation_transport_unavailable())
    }

    fn queue_federation_executor_operation(
        &self, operation: &FederationExecutorOperation, live_wake: bool,
    ) -> Result<(), ApiError> {
        self.queue_federation_executor_operation_inner(operation, live_wake, true)
    }

    fn queue_federation_executor_operation_inner(
        &self, operation: &FederationExecutorOperation, live_wake: bool, activate: bool,
    ) -> Result<(), ApiError> {
        operation.validate().map_err(mailbox_api_error)?;
        if operation.command_kind != "wake" {
            return Err(ApiError::conflict("lifecycle operations require separate reconciliation"));
        }
        let wake: FederationExecutorWake = serde_json::from_str(&operation.payload)
            .map_err(|_| ApiError::bad_request("invalid retained wake metadata"))?;
        wake.validate()?;
        if live_wake {
            // Receive already committed this operation. Capture the same-boot
            // activation before any queue-store read/write can fail.
            self.retain_federation_executor_live_activation(operation)?;
        }
        let prompt_id = operation.queue_identity();
        let stored = {
            let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
            federation_executor_read(&connection, &operation.central_host_id, &operation.operation_key)
                .map_err(mailbox_api_error)?
                .ok_or_else(|| ApiError::conflict("wake has no received journal entry"))?
        };
        if stored.operation != *operation { return Err(ApiError::conflict("wake journal content changed")); }
        if stored.effect_state != "not_started" {
            self.federation_executor_wake_signal.live.lock().expect("live activation mutex poisoned")
                .remove(&Self::federation_executor_live_key(operation));
        }
        #[cfg(test)]
        federation_executor_test_cut("beforeQueueAssociation").map_err(mailbox_api_error)?;
        let (nonce, content, attempt) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            federation_executor_target_locked(&inner, operation)?;
            let index = inner.find_session_index(&operation.session_id).expect("binding checked");
            let record = inner.session_mut_by_index(index).expect("binding checked");
            if let Some(receipt) = record.prompt_deliveries.get(&prompt_id) {
                if receipt.federation_operation.as_ref() != Some(operation) {
                    return Err(ApiError::conflict("remote queue identity conflicts"));
                }
                if receipt.disposition != PromptDeliveryDisposition::Pending {
                    // Accepted, ambiguous, rejected or handed-off evidence is
                    // never permission to recreate a queue entry.
                    let accepted = receipt.disposition == PromptDeliveryDisposition::Accepted;
                    drop(inner);
                    self.federation_executor_wake_signal.live.lock().expect("live activation mutex poisoned")
                        .remove(&Self::federation_executor_live_key(operation));
                    if accepted {
                        self.sync_federation_executor_runtime_receipt(&operation.session_id, &prompt_id);
                    }
                    return Ok(());
                }
                if !record.queued_prompts.iter().any(|entry| entry.pending_prompt.id == prompt_id) {
                    return Err(ApiError::conflict("remote queue association was retired")
                        .with_kind(ApiErrorKind::FederationExecutorAssociationRetired));
                }
                if stored.queue_entry_id.as_deref() == Some(prompt_id.as_str())
                    && stored.delivery_attempt_id.as_deref() == Some(prompt_id.as_str())
                    && record.queued_prompt_durability_holds.get(&prompt_id).is_some_and(|hold|
                        hold.acknowledged && record.queued_prompts.iter().any(|entry|
                            entry.pending_prompt.id == prompt_id
                                && queued_prompt_durability_content(entry) == hold.content))
                {
                    // Both durability surfaces already agree. A periodic scan
                    // must not republish a paused queue or rewrite its row.
                    drop(inner);
                    if activate { self.dispatch_federation_executor_live_activation(operation); }
                    return Ok(());
                }
            } else {
                if stored.queue_entry_id.is_some() || stored.effect_state != "not_started" {
                    return Err(ApiError::conflict("remote delivery evidence is missing; reconcile"));
                }
                let source = MessageSource::mailbox(wake.sender_session_id.clone(), wake.sender_name.clone(),
                    MailboxMessageSource {
                        mailbox_id: wake.mailbox_id.clone(), message_id: wake.message_id.clone(),
                        sequence: wake.sequence, unread_count: wake.unread_count,
                    });
                queue_receipted_orchestrator_prompt_on_record(record, PendingPrompt {
                    id: prompt_id.clone(), timestamp: stamp_now(),
                    text: mailbox_notification_text(&wake.mailbox_id, wake.unread_count, wake.sequence,
                        &wake.sender_name, wake.topic.as_deref()),
                    expanded_text: None, source: Some(source), attachments: Vec::new(),
                    engram_interrupted: false, is_engram_retained: false,
                }, Vec::new());
                let queued = record.queued_prompts.back_mut().expect("just queued");
                queued.source = QueuedPromptSource::Mailbox;
                let content = queued_prompt_durability_content(queued);
                record.queued_prompt_durability_holds.get_mut(&prompt_id).expect("just held").content = content;
                let receipt = record.prompt_deliveries.get_mut(&prompt_id).expect("just receipted");
                receipt.federation_operation = Some(operation.clone());
            }
            let receipt = record.prompt_deliveries.get(&prompt_id).expect("association retained");
            let attempt = receipt.write_ahead.clone();
            let hold = record.queued_prompt_durability_holds.get(&prompt_id)
                .ok_or_else(|| ApiError::conflict("remote queue lost its content hold"))?;
            let nonce = hold.nonce.clone();
            let content = hold.content.clone();
            self.commit_locked(&mut inner).map_err(|_| federation_transport_unavailable())?;
            (nonce, content, attempt)
        };
        self.prove_federation_queue_content(PersistFenceTarget::FederationExecutorQueue {
            session_id: operation.session_id.clone(), prompt_id: prompt_id.clone(),
            content: queued_prompt_durability_fence_content(&nonce, &content),
            operation: Box::new(operation.clone()), write_ahead: attempt.clone(),
        })?;
        #[cfg(test)]
        federation_executor_test_cut("afterQueueAssociationCommit").map_err(mailbox_api_error)?;
        let authority = FederationExecutorAuthority { app: self, operation, lease: None };
        let host = self.federation_executor_host_id().map_err(mailbox_api_error)?;
        self.mailbox_store.record_federation_executor_evidence(&host, operation,
            FederationExecutorEvidence::Queued { queue_entry_id: prompt_id.clone(), delivery_attempt_id: prompt_id.clone() },
            || authority.lock()).map_err(mailbox_api_error)?;
        #[cfg(test)]
        federation_executor_test_cut("afterQueueEvidenceCommit").map_err(mailbox_api_error)?;
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            federation_executor_target_locked(&inner, operation)?;
            let index = inner.find_session_index(&operation.session_id).expect("binding checked");
            let record = inner.session_mut_by_index(index).expect("binding checked");
            let exact_entry = record.queued_prompts.iter().find(|q| q.pending_prompt.id == prompt_id)
                .is_some_and(|q| queued_prompt_durability_content(q) == content);
            let hold = record.queued_prompt_durability_holds.get_mut(&prompt_id)
                .filter(|hold| hold.nonce == nonce && hold.content == content && exact_entry)
                .ok_or_else(|| ApiError::conflict("remote queue changed while saving"))?;
            if !hold.acknowledged || hold.failure.is_some() {
                hold.acknowledged = true;
                hold.failure = None;
                self.commit_locked(&mut inner).map_err(|_| federation_transport_unavailable())?;
            }
        }
        #[cfg(test)]
        federation_executor_test_cut("afterQueueDurabilityAck").map_err(mailbox_api_error)?;
        // Boot recovery materializes evidence but never resumes dormant queues.
        if activate { self.dispatch_federation_executor_live_activation(operation); }
        Ok(())
    }
}
