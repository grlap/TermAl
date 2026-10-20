// Owns journal write-ahead and positive runtime receipt propagation.
// Storage waits run off the state lock; the final provider enqueue rechecks
// native identity under the same lock that arbitrates Stop and runtime tokens.

#[derive(Clone)]
struct FederationExecutorHandoffIdentity {
    operation: FederationExecutorOperation,
    prompt_id: String,
    write_ahead: String,
    server_instance_id: String,
}

impl FederationExecutorHandoffIdentity {
    fn matches(&self, record: &SessionRecord) -> bool {
        record.prompt_deliveries.get(&self.prompt_id).is_some_and(|delivery|
            delivery.write_ahead == self.write_ahead
                && delivery.federation_operation.as_ref() == Some(&self.operation)
                && delivery.disposition == PromptDeliveryDisposition::HandingOff)
    }
}

impl AppState {
    fn reconcile_federation_executor_runtime_receipt(&self, journal: &FederationExecutorRecord)
        -> Result<(), ApiError>
    {
        let prompt = journal.operation.queue_identity();
        if journal.effect_state != "confirmed"
            || journal.runtime_ack.as_deref() != Some(format!("runtime-accepted:{prompt}").as_str())
            || journal.queue_entry_id.as_deref() != Some(prompt.as_str())
        { return Err(ApiError::conflict("runtime receipt does not match the retained operation")); }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        federation_executor_target_locked(&inner, &journal.operation)?;
        let index = inner.find_session_index(&journal.operation.session_id).expect("binding checked");
        let record = inner.session_mut_by_index(index).expect("binding checked");
        let Some(receipt) = record.prompt_deliveries.get_mut(&prompt) else { return Ok(()); };
        if receipt.federation_operation.as_ref() != Some(&journal.operation) {
            return Err(ApiError::conflict("runtime receipt association changed"));
        }
        if receipt.disposition == PromptDeliveryDisposition::Accepted { return Ok(()); }
        receipt.disposition = PromptDeliveryDisposition::Accepted;
        receipt.reason = None;
        receipt.entry = None;
        record.queued_prompts.retain(|queued| queued.pending_prompt.id != prompt);
        record.queued_prompt_durability_holds.remove(&prompt);
        sync_prompt_delivery_view(record);
        self.commit_locked(&mut inner).map_err(|_| federation_transport_unavailable())?;
        Ok(())
    }

    fn retain_federation_executor_mailbox_ack(
        &self, binding: &FederationMailboxBinding, summary: &MailboxSummary,
        lease: &FederationLinkLease,
    ) -> Result<(), ApiError> {
        let actor = binding.participant_key();
        let processed = summary.participants.iter().find(|p| p.session_id == actor)
            .ok_or_else(|| ApiError::bad_gateway("central ACK omitted its participant"))?.processed_through;
        let host = self.federation_executor_host_id().map_err(mailbox_api_error)?;
        let mut after = String::new();
        loop {
            let rows = self.mailbox_store.pending_federation_executor_operations(
                &binding.central.host_id, &after, 100).map_err(mailbox_api_error)?;
            if rows.is_empty() { break; }
            for row in rows {
                after = row.operation.operation_key.clone();
                if row.operation.target_host_id != host || row.operation.mailbox_binding() != *binding
                    || row.operation.command_kind != "wake" { continue; }
                let wake: FederationExecutorWake = serde_json::from_str(&row.operation.payload)
                    .map_err(|_| ApiError::conflict("retained wake metadata is invalid"))?;
                if wake.mailbox_id != summary.id || wake.sequence > processed { continue; }
                if federation_executor_operation_retired(&self.inner.lock().expect("state mutex poisoned"), &row.operation) {
                    continue;
                }
                let authority = FederationExecutorAuthority { app: self, operation: &row.operation, lease: Some(lease) };
                let result = self.mailbox_store.record_federation_executor_evidence(&host, &row.operation,
                    FederationExecutorEvidence::Processed {
                        kind: FederationExecutorProcessedKind::MailboxAck,
                        evidence: format!("central-mailbox-ack:{}:{}", summary.id, wake.sequence),
                    }, || authority.lock()).map_err(mailbox_api_error);
                if let Err(error) = result {
                    // Re-enrollment can win after the precheck. Only the same
                    // permanent per-row conflict is excluded; storage errors
                    // and an old lease for a current operation still refuse.
                    if error.status == StatusCode::CONFLICT
                        && federation_executor_operation_retired(
                            &self.inner.lock().expect("state mutex poisoned"), &row.operation)
                    { continue; }
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn prepare_federation_executor_handoff(
        &self, session: &str, token: &RuntimeToken, turn: u64,
        identity: Option<&FederationExecutorHandoffIdentity>,
    ) -> Result<Option<(FederationExecutorOperation, String, bool)>, ApiError> {
        // Only promotion of this exact receipted prompt can attach an identity.
        // Ordinary turns cannot collide with retained receipts after a reload.
        let Some(identity) = identity else { return Ok(None); };
        if identity.server_instance_id != self.server_instance_id
            || identity.operation.session_id != session
            || identity.operation.queue_identity() != identity.prompt_id
        { return Err(ApiError::conflict("remote handoff identity changed")); }
        let captured = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(session)
                .ok_or_else(|| ApiError::conflict("remote handoff session is missing"))?;
            let record = &inner.sessions[index];
            let delivery = record.prompt_deliveries.get(&identity.prompt_id)
                .ok_or_else(|| ApiError::conflict("remote handoff receipt is missing"))?;
            if !record.runtime.matches_runtime_token(token)
                || record.active_turn_generation != turn
                || delivery.turn_generation != Some(turn)
                || delivery.disposition != PromptDeliveryDisposition::HandingOff
                || delivery.federation_operation.as_ref() != Some(&identity.operation)
                || delivery.write_ahead != identity.write_ahead
            {
                return Err(ApiError::conflict("remote handoff is no longer current"));
            }
            let operation = delivery.federation_operation.clone().expect("checked");
            federation_executor_target_locked(&inner, &operation)?;
            (operation, delivery.write_ahead.clone(), delivery.federation_handoff_started)
        };
        // A second carrier for this exact live attempt is already owned by
        // the provider channel. Leave its receipt unresolved; never enqueue
        // again or turn the original in-flight attempt into a rejection.
        if captured.2 { return Ok(Some(captured)); }
        let prompt = captured.0.queue_identity();
        self.prove_federation_queue_content(PersistFenceTarget::FederationExecutorHandoff {
            session_id: session.to_owned(), prompt_id: prompt.clone(),
            operation: Box::new(captured.0.clone()), write_ahead: captured.1.clone(),
        })?;
        let authority = FederationExecutorAuthority { app: self, operation: &captured.0, lease: None };
        let host = self.federation_executor_host_id().map_err(mailbox_api_error)?;
        // Recovery may not yet have reached this row. The persisted receipt
        // proves its queue association, so repair that gap before EffectAttempt.
        self.mailbox_store.record_federation_executor_evidence(&host, &captured.0,
            FederationExecutorEvidence::Queued { queue_entry_id: prompt.clone(), delivery_attempt_id: prompt },
            || authority.lock()).map_err(mailbox_api_error)?;
        // The journal pins the one logical operation-derived effect identity.
        // PromptDelivery keeps its own fresh nonce for an unsent local retry,
        // so late durability callbacks cannot authorize a newer handoff.
        self.mailbox_store.record_federation_executor_evidence(&host, &captured.0,
            FederationExecutorEvidence::EffectAttempt { delivery_attempt_id: captured.0.queue_identity() },
            || authority.lock()).map_err(mailbox_api_error)?;
        Ok(Some(captured))
    }

    fn sync_federation_executor_runtime_receipt(&self, session: &str, prompt_id: &str) {
        let captured = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session) else { return; };
            let Some(delivery) = inner.sessions[index].prompt_deliveries.get(prompt_id) else { return; };
            if delivery.disposition != PromptDeliveryDisposition::Accepted { return; }
            let Some(operation) = delivery.federation_operation.clone() else { return; };
            (operation, delivery.write_ahead.clone())
        };
        let authority = FederationExecutorAuthority { app: self, operation: &captured.0, lease: None };
        let Ok(host) = self.federation_executor_host_id() else { return; };
        if let Err(error) = self.mailbox_store.record_federation_executor_evidence(&host, &captured.0,
            FederationExecutorEvidence::RuntimeAccepted {
                delivery_attempt_id: captured.0.queue_identity(),
                acknowledgement: format!("runtime-accepted:{prompt_id}"),
            }, || authority.lock())
        {
            eprintln!("federation runtime receipt retained in session for reconciliation: {error:#}");
        }
    }
}
