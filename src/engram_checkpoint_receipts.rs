// Owns bounded volatile custody of exact checkpoint replies awaiting local ACK.
// Never serialized, an admission permit, a replay scheduler, or crash evidence.

// A JSON string can expand to six ASCII bytes per input byte when escaped.
// The transport frame also contains an envelope, so this is a conservative
// retained-Value bound, enforced again for adapters that return Value directly.
const ENGRAM_CHECKPOINT_REPLY_MAX_BYTES: usize = 6 * ENGRAM_CONTROL_MAX_FRAME_BYTES;
const ENGRAM_CHECKPOINT_PENDING_MAX_SLOTS: usize = 16;
const ENGRAM_CHECKPOINT_PENDING_MAX_BYTES: usize = 64 * 1024 * 1024;

struct EngramBoundedJson {
    bytes: Vec<u8>,
    limit: usize,
}

impl std::io::Write for EngramBoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other(
                "checkpoint representation capacity exceeded",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn bounded_checkpoint_json(value: &impl Serialize, limit: usize) -> Result<Arc<[u8]>> {
    let mut writer = EngramBoundedJson {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes.into())
}

#[derive(Clone)]
struct EngramPendingCheckpointReceipt {
    origin: EngramCheckpointReceiptOrigin,
    identity: Arc<[u8]>,
    reserved_bytes: usize,
    response: Option<Arc<[u8]>>,
    accepted: bool,
    accounting: bool,
    card: Option<Arc<[u8]>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EngramCheckpointReceiptOrigin {
    Ordinary,
    OffContainment,
    OriginalRecovery,
}

#[derive(Default)]
struct EngramPendingCheckpointReceipts {
    entries: BTreeMap<(String, String), EngramPendingCheckpointReceipt>,
    reserved_bytes: usize,
}

fn checkpoint_receipt_identity(operation: &EngramCheckpointOperation) -> Result<Arc<[u8]>> {
    let attempt_id = &operation
        .attempts
        .last()
        .ok_or_else(|| anyhow!("missing checkpoint attempt"))?
        .id;
    checkpoint_receipt_identity_through_attempt(operation, attempt_id)
}

fn checkpoint_receipt_identity_through_attempt(
    operation: &EngramCheckpointOperation,
    attempt_id: &str,
) -> Result<Arc<[u8]>> {
    let index = operation
        .attempts
        .iter()
        .position(|attempt| attempt.id == attempt_id)
        .ok_or_else(|| anyhow!("checkpoint attempt disappeared"))?;
    // A later attempt may exist, but cannot change the original request or
    // any provenance through the attempt whose reply is being accounted.
    let mut attempts = operation.attempts[..=index].to_vec();
    let attempt = attempts
        .last_mut()
        .ok_or_else(|| anyhow!("missing checkpoint attempt"))?;
    // Result is progress, not identity. All original request and caller owner
    // fields remain in this immutable, bounded identity image.
    attempt.result = None;
    bounded_checkpoint_json(
        &(&operation.original, attempts),
        2 * ENGRAM_SESSION_OBLIGATION_REQUEST_MAX_BYTES,
    )
}

fn checkpoint_receipt_key(operation: &EngramCheckpointOperation) -> (String, String) {
    (
        operation.original.id.clone(),
        operation
            .attempts
            .last()
            .expect("validated attempt")
            .id
            .clone(),
    )
}

// Resume only the local ACK of an already committed retirement. The existing
// reservation proves custody of this accepted reply; it grants no new send,
// head retirement, provider owner or release of another obligation.
fn retired_checkpoint_receipt_matches(
    state: &AppState,
    inner: &StateInner,
    session_id: &str,
    operation: &EngramCheckpointOperation,
    reservation: &EngramCheckpointReceiptReservation,
) -> bool {
    let Some(attempt) = operation.attempts.last() else { return false };
    let Some(prompt_id) = attempt.queued_prompt_id.as_ref() else { return false };
    let EngramExistingCheckpointProgress::Accepted { response } = &operation.progress
        else { return false };
    let EngramControlRequest::TurnCheckpoint { routing_token, next_intent, .. } =
        &operation.original.request else { return false };
    let Some(entry) = inner.engram_pending_checkpoint_receipts.entries.get(&reservation.key)
        else { return false };
    let Some(index) = inner.find_session_index(session_id) else { return false };
    let record = &inner.sessions[index];
    let canonical_matches = inner.engram_session_obligations.checkpoint_operations.iter()
        .find(|current| current.original.id == operation.original.id)
        .is_some_and(|current| matches!(
            (serde_json::to_value(current), serde_json::to_value(operation)),
            (Ok(current), Ok(expected)) if current == expected
        ));
    Arc::ptr_eq(&state.inner, &reservation.state.inner)
        && reservation.key == checkpoint_receipt_key(operation)
        && entry.origin == EngramCheckpointReceiptOrigin::Ordinary
        && entry.accepted && entry.accounting && entry.card.is_some()
        && checkpoint_receipt_identity(operation).is_ok_and(|identity| identity == entry.identity)
        && entry.response.as_deref().is_some_and(|bytes| {
            serde_json::from_slice::<Value>(bytes).is_ok_and(|value| &value == response)
        })
        && canonical_matches && operation.validate().is_ok()
        && operation.protection.phase == EngramSessionObligationPhase::Confirmed
        && matches!(attempt.result, Some(EngramCheckpointAttemptResult::Accepted))
        && *next_intent == EngramNextIntent::Wait
        && attempt.dispatch_generation.checked_add(1) == Some(record.engram.dispatch_generation)
        && record.active_turn_generation == attempt.active_turn_generation
        && record.engram.routing_token.as_ref() == Some(routing_token)
        && record.engram.work_binding == operation.original.work_binding
        && record.engram.active_grant_id.is_none()
        && record.engram.uncertain_grant_id.is_none()
        && record.engram.pending_dispatch.is_none()
        && record.engram.admission_in_progress.is_none()
        && !record.engram.bind_in_progress && !record.engram.project_reset_in_progress
        && !record.engram.checkpoint_in_progress && record.engram.checkpoint_status_owner.is_none()
        && !record.runtime_stop_in_progress && record.runtime_stop_owner.is_none()
        && matches!(record.runtime, SessionRuntime::None)
        && !matches!(record.session.status,
            SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping)
        && !record.queued_prompts.iter().any(|queued| {
            &queued.pending_prompt.id == prompt_id || queued.has_engram_intent()
        })
}

impl EngramPendingCheckpointReceipts {
    fn reserve(&mut self, operation: &EngramCheckpointOperation) -> Result<()> {
        let identity = checkpoint_receipt_identity(operation)?;
        let key = checkpoint_receipt_key(operation);
        self.reserve_identity(key, identity, EngramCheckpointReceiptOrigin::Ordinary)
    }

    fn reserve_identity(
        &mut self,
        key: (String, String),
        identity: Arc<[u8]>,
        origin: EngramCheckpointReceiptOrigin,
    ) -> Result<()> {
        // One operation cannot acquire a second attempt while any reply or
        // send reservation for its original identity remains in custody.
        if self.entries.keys().any(|(id, _)| id == &key.0) {
            bail!("checkpoint receipt is already reserved or awaits local accounting");
        }
        // Includes immutable identity, map key allocation, card and response.
        // Durable receipt copies are separately counted by full prospective
        // ledger and per-image validation, never charged as one response copy.
        let bytes = identity
            .len()
            .checked_mul(2)
            .and_then(|n| n.checked_add(ENGRAM_CHECKPOINT_REPLY_MAX_BYTES))
            .and_then(|n| n.checked_add(ENGRAM_SESSION_OBLIGATION_REQUEST_MAX_BYTES))
            .and_then(|n| {
                n.checked_add(std::mem::size_of::<EngramPendingCheckpointReceipt>() + 256)
            })
            .ok_or_else(|| anyhow!("checkpoint receipt reservation overflow"))?;
        if self.entries.len() >= ENGRAM_CHECKPOINT_PENDING_MAX_SLOTS
            || bytes > ENGRAM_CHECKPOINT_PENDING_MAX_BYTES.saturating_sub(self.reserved_bytes)
        {
            bail!("local checkpoint receipt capacity is unavailable; nothing sent");
        }
        self.entries.insert(
            key,
            EngramPendingCheckpointReceipt {
                origin,
                identity,
                reserved_bytes: bytes,
                response: None,
                accepted: false,
                accounting: true,
                card: None,
            },
        );
        self.reserved_bytes += bytes;
        Ok(())
    }

    fn receive(
        &mut self,
        operation: &EngramCheckpointOperation,
        response: Arc<[u8]>,
        accepted: bool,
    ) -> Result<()> {
        if self
            .entries
            .get(&checkpoint_receipt_key(operation))
            .is_none_or(|entry| entry.origin != EngramCheckpointReceiptOrigin::Ordinary)
        {
            bail!("ordinary checkpoint receipt reservation owner changed");
        }
        self.receive_identity(
            &checkpoint_receipt_key(operation),
            &checkpoint_receipt_identity(operation)?,
            response,
            accepted,
        )
    }

    fn receive_identity(
        &mut self,
        key: &(String, String),
        identity: &[u8],
        response: Arc<[u8]>,
        accepted: bool,
    ) -> Result<()> {
        if response.len() > ENGRAM_CHECKPOINT_REPLY_MAX_BYTES {
            bail!("unsupported checkpoint response size");
        }
        let entry = self
            .entries
            .get_mut(key)
            .filter(|entry| entry.identity.as_ref() == identity)
            .ok_or_else(|| anyhow!("checkpoint receipt reservation owner changed"))?;
        if let Some(previous) = &entry.response {
            if previous.as_ref() != response.as_ref() || entry.accepted != accepted {
                bail!("conflicting checkpoint response cannot replace retained receipt");
            }
            return Ok(());
        }
        entry.response = Some(response);
        entry.accepted = accepted;
        Ok(())
    }

    fn remove(&mut self, key: &(String, String)) {
        if let Some(entry) = self.entries.remove(key) {
            self.reserved_bytes -= entry.reserved_bytes;
        }
    }
}

struct EngramCheckpointReceiptReservation {
    state: AppState,
    key: (String, String),
}

// Administrative removal preserves debt but must leave a live send/accounting
// owner its row. An idle stale reservation is historical custody, not a caller.
fn checkpoint_receipt_has_live_owner(inner: &StateInner, operation_id: &str) -> bool {
    inner.engram_pending_checkpoint_receipts.entries.iter().any(|(key, pending)| {
        if key.0 != operation_id { return false; }
        if pending.accounting { return true; }
        match pending.origin {
            EngramCheckpointReceiptOrigin::OriginalRecovery => false,
            EngramCheckpointReceiptOrigin::Ordinary => {
                let Some(operation) = inner.engram_session_obligations.checkpoint_operations.iter()
                    .find(|operation| operation.original.id == key.0)
                else { return true; };
                if !checkpoint_receipt_identity_through_attempt(operation, &key.1)
                    .is_ok_and(|identity| identity == pending.identity)
                { return true; }
                existing_checkpoint_accounting_owner_matches(inner,
                    &operation.protection.local_session_id, &operation.original, &key.1, false)
            }
            EngramCheckpointReceiptOrigin::OffContainment => {
                let Ok(owner) = serde_json::from_slice::<EngramContainmentReceiptOwner>(&pending.identity)
                else { return true; };
                let response = match pending.response.as_ref()
                    .map(|response| serde_json::from_slice::<Value>(response)).transpose()
                {
                    Ok(response) => response,
                    Err(_) => return true,
                };
                off_checkpoint_receipt_owner_matches(inner, &owner, response.as_ref(), false)
                    .unwrap_or(true)
            }
        }
    })
}

impl Drop for EngramCheckpointReceiptReservation {
    fn drop(&mut self) {
        let Ok(mut inner) = self.state.inner.lock() else {
            return;
        };
        if inner
            .engram_pending_checkpoint_receipts
            .entries
            .get(&self.key)
            .is_some_and(|entry| !entry.accepted)
        {
            inner.engram_pending_checkpoint_receipts.remove(&self.key);
        } else if let Some(entry) = inner
            .engram_pending_checkpoint_receipts
            .entries
            .get_mut(&self.key)
        {
            entry.accounting = false;
        }
    }
}

impl AppState {
    #[cfg(test)]
    fn note_checkpoint_receipt_capacity(&self, expected: &EngramCheckpointOperation) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let mut ledger = inner.engram_session_obligations.clone();
        let Some(operation) = ledger
            .checkpoint_operations
            .iter_mut()
            .find(|operation| operation.original.id == expected.original.id)
        else {
            return;
        };
        let (Ok(current), Ok(original)) = (
            checkpoint_receipt_identity(operation),
            checkpoint_receipt_identity(expected),
        ) else {
            return;
        };
        if current != original {
            return;
        }
        operation.protection.reason_code = "checkpoint_receipt_capacity_unavailable".into();
        let mirror = operation.clone();
        if ledger.validate().is_err() {
            return;
        }
        inner.engram_session_obligations = ledger;
        if let Some(index) = inner.find_session_index(&expected.original.connection.session_id)
            && inner.sessions[index]
                .engram
                .checkpoint_operation
                .as_ref()
                .is_some_and(|operation| operation.original.id == expected.original.id)
        {
            inner.session_mut_by_index(index).expect("checkpoint mirror owner")
                .engram.checkpoint_operation = Some(mirror);
        }
        let _ = self.commit_locked(&mut inner);
    }

    #[cfg(test)]
    fn reserve_checkpoint_receipt(
        &self,
        operation: &EngramCheckpointOperation,
    ) -> Result<EngramCheckpointReceiptReservation> {
        self.inner
            .lock()
            .expect("state mutex poisoned")
            .engram_pending_checkpoint_receipts
            .reserve(operation)?;
        Ok(EngramCheckpointReceiptReservation {
            state: self.clone(),
            key: checkpoint_receipt_key(operation),
        })
    }

    // Existing explicit local accounting only. Does not retransmit a request or
    // grant a fresh owner permission; finish rechecks the exact original owner.
    fn account_pending_checkpoint_receipt(&self, session_id: &str) -> Result<bool> {
        let pending = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return Ok(false);
            };
            let Some(operation) = inner.sessions[index].engram.checkpoint_operation.as_ref() else {
                return Ok(false);
            };
            let operation = operation.clone();
            let Some(entry) = inner
                .engram_pending_checkpoint_receipts
                .entries
                .get_mut(&checkpoint_receipt_key(&operation))
                .filter(|entry| {
                    entry.origin == EngramCheckpointReceiptOrigin::Ordinary && entry.accepted
                })
            else {
                return Ok(false);
            };
            if entry.accounting {
                bail!("checkpoint receipt accounting is already active");
            }
            if entry.identity != checkpoint_receipt_identity(&operation)? {
                bail!("pending checkpoint accounting owner changed");
            }
            entry.accounting = true;
            (operation, entry.response.clone(), entry.card.clone())
        };
        let (operation, response, card) = pending;
        let reservation = EngramCheckpointReceiptReservation {
            state: self.clone(),
            key: checkpoint_receipt_key(&operation),
        };
        let response: Value = serde_json::from_slice(
            &response.ok_or_else(|| anyhow!("pending receipt is incomplete"))?,
        )?;
        let card: EngramControlCard = serde_json::from_slice(
            &card.ok_or_else(|| anyhow!("pending receipt accounting is still active"))?,
        )?;
        let EngramControlRequest::TurnCheckpoint {
            grant_id, report, ..
        } = &operation.original.request
        else {
            bail!("not a checkpoint")
        };
        let deadline = self
            .checkpoint_preflight_budget(session_id)
            .map(|b| b.0)
            .unwrap_or_else(|| self.engram_budget_clock().now())
            + operation.original.settings.call_timeout();
        if matches!(
            operation.progress,
            EngramExistingCheckpointProgress::Accepted { .. }
        ) {
            if !self.acknowledge_existing_checkpoint_settlement_with_owners(
                session_id, &operation, deadline, None, Some(&reservation),
            ) {
                bail!("checkpoint receipt durable acknowledgement remains pending");
            }
            self.release_checkpoint_receipt_custody(&operation);
            return Ok(true);
        }
        if !self.finish_engram_checkpoint_record_with_operation(
            session_id,
            grant_id,
            EngramControlCardDecision::Grant,
            card,
            operation.original.checkpoint_owner_generation,
            engram_turn_report_observes_turn(report, session_id, grant_id),
            Some(&operation),
            Some(response),
            Some(deadline),
            Some(EngramCheckpointAttemptResult::Accepted),
        ) {
            bail!("checkpoint receipt received but local durable accounting remains pending");
        }
        Ok(true)
    }

    // Call only after the prospective canonical ledger/receipt has validated,
    // and before its first Accepted persistence. A historical receipt proves
    // its own evidence identities, never ownership of a successor's batches.
    fn settle_checkpoint_receipt_effects_locked(
        inner: &mut StateInner,
        operation: &EngramCheckpointOperation,
        response: &Value,
        owns_session: bool,
    ) -> Result<Vec<TestRunCommandObservation>> {
        let EngramTurnCheckpointResponse::Checkpointed { receipt } =
            parse_engram_result(response.clone()).map_err(|error| anyhow!("{error}"))?
            else { bail!("Not a checkpoint receipt") };
        let EngramControlRequest::TurnCheckpoint { grant_id, report, .. } = &operation.original.request
            else { bail!("Not a checkpoint") };
        if owns_session {
            Self::owe_checkpoint_batch_binds_locked(inner, &operation.protection.local_session_id, operation, &receipt)?;
        }
        let mut changed = Vec::new();
        if let Some((store, binding)) = operation
            .original
            .settings
            .authority_store_key
            .as_ref()
            .zip(operation.original.work_binding.as_ref())
        {
            changed = Self::settle_test_run_verification_receipt_locked(inner, report, &receipt, store, binding);
        }
        settle_engram_pending_close_by_checkpoint_locked(inner, grant_id,
            engram_turn_report_observes_turn(report, &operation.protection.local_session_id, grant_id));
        Ok(changed)
    }

    // All durable effects are already in the accepted cut. Volatile custody
    // can be discarded only after its writer proof succeeds.
    fn release_checkpoint_receipt_custody(&self, operation: &EngramCheckpointOperation) {
        self.inner
            .lock()
            .expect("state mutex poisoned")
            .engram_pending_checkpoint_receipts
            .remove(&checkpoint_receipt_key(operation));
    }
}

#[cfg(test)]
fn prospective_checkpoint_result(
    inner: &StateInner,
    session_id: &str,
    expected: &EngramCheckpointOperation,
    response: Option<&Value>,
    result: Option<&EngramCheckpointAttemptResult>,
) -> Result<(EngramSessionObligationLedger, bool, EngramCheckpointOperation)> {
    prospective_checkpoint_result_with_rejected_owner(inner, session_id, expected, response, result, None)
}

fn prospective_checkpoint_result_with_rejected_owner(
    inner: &StateInner,
    session_id: &str,
    expected: &EngramCheckpointOperation,
    response: Option<&Value>,
    result: Option<&EngramCheckpointAttemptResult>,
    rejected_delivery_owner: Option<&RejectedEngramCheckpointOwner>,
) -> Result<(
    EngramSessionObligationLedger,
    bool,
    EngramCheckpointOperation,
)> {
    prospective_checkpoint_result_with_live_owner(inner, session_id, expected, response,
        result, rejected_delivery_owner, true)
}

// Recovery shares result custody/validation but supplies its own current owner.
// It must not inherit a weaker generic live-owner or adopted-duty release.
fn prospective_checkpoint_result_with_live_owner(
    inner: &StateInner,
    session_id: &str,
    expected: &EngramCheckpointOperation,
    response: Option<&Value>,
    result: Option<&EngramCheckpointAttemptResult>,
    rejected_delivery_owner: Option<&RejectedEngramCheckpointOwner>,
    allow_live_owner: bool,
) -> Result<(EngramSessionObligationLedger, bool, EngramCheckpointOperation)> {
    let attempt_id = &expected
        .attempts
        .last()
        .ok_or_else(|| anyhow!("missing attempt"))?
        .id;
    let canonical = inner
        .engram_session_obligations
        .checkpoint_operations
        .iter()
        .find(|operation| operation.original.id == expected.original.id)
        .ok_or_else(|| anyhow!("original checkpoint protection disappeared"))?;
    if checkpoint_receipt_identity_through_attempt(canonical, attempt_id)?
        != checkpoint_receipt_identity(expected)?
    {
        bail!("checkpoint original or attempt owner changed");
    }
    let owns = allow_live_owner && existing_checkpoint_accounting_owner_matches(
        inner,
        session_id,
        &expected.original,
        attempt_id,
        false,
    ) && rejected_delivery_owner.is_none_or(|owner| {
        inner.find_session_index(session_id).is_some_and(|index| {
            rejected_checkpoint_head_matches(&inner.sessions[index], expected, owner, false)
        })
    });
    let mut ledger = inner.engram_session_obligations.clone();
    let operation = ledger
        .checkpoint_operations
        .iter_mut()
        .find(|operation| operation.original.id == expected.original.id)
        .expect("canonical");
    if let Some(result) = result {
        let attempt = operation
            .attempts
            .iter_mut()
            .find(|attempt| &attempt.id == attempt_id)
            .expect("matched attempt prefix");
        if attempt.result.as_ref().is_some_and(|previous| {
            serde_json::to_value(previous).ok() != serde_json::to_value(result).ok()
        }) {
            bail!("conflicting checkpoint attempt result");
        }
        attempt.result = Some(result.clone());
    }
    if let Some(response) = response {
        if let EngramExistingCheckpointProgress::Accepted { response: previous } =
            &operation.progress
            && previous != response
        {
            bail!("conflicting accepted checkpoint receipt");
        }
        operation.progress = EngramExistingCheckpointProgress::Accepted {
            response: response.clone(),
        };
        operation.protection.settlement_evidence = response.clone();
        if owns {
            let record = &inner.sessions[inner.find_session_index(session_id).expect("owner")];
            let only_current_duty = record.engram.uncertain_grant_id.is_none()
                && !record.engram.bind_in_progress
                && record.engram.pending_dispatch.is_none()
                && !record.runtime_stop_in_progress
                && !matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
                )
                && !record
                    .queued_prompts
                    .iter()
                    .any(QueuedPromptRecord::has_engram_intent);
            for entry in &mut ledger.entries {
                if entry.local_session_id != session_id {
                    continue;
                }
                let Ok(image) = serde_json::from_value::<EngramUnsettledControlImage>(
                    entry.frozen_request.clone(),
                ) else {
                    continue;
                };
                if !image
                    .existing_checkpoint
                    .as_ref()
                    .is_some_and(|operation| operation.original.id == expected.original.id)
                {
                    continue;
                }
                entry.settlement_evidence =
                    serde_json::to_value(EngramContainedCheckpointProgress {
                        operation: "existing_checkpoint_progress".into(),
                        checkpoint_id: expected.original.id.clone(),
                        owner_generation: entry.owner_generation,
                        response: response.clone(),
                    })?;
                if entry.authority_confirmed
                    && only_current_duty
                    && contained_checkpoint_is_only_duty(&image)
                {
                    entry.phase = EngramSessionObligationPhase::Confirmed;
                }
            }
            if ledger
                .entries
                .iter()
                .filter(|entry| entry.id == expected.original.id)
                .all(|entry| entry.phase == EngramSessionObligationPhase::Confirmed)
            {
                operation.protection.phase = EngramSessionObligationPhase::Confirmed;
            }
        }
    }
    // Checks all enclosing frozen-request bounds as well as every duplicate
    // identity and the aggregate size of every durable receipt copy.
    ledger.validate()?;
    let canonical = ledger
        .checkpoint_operations
        .iter()
        .find(|operation| operation.original.id == expected.original.id)
        .expect("canonical")
        .clone();
    Ok((ledger, owns, canonical))
}
