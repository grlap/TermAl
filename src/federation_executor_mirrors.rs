// Central ownership receipts reuse immutable v4 operation identity. A mirror
// never carries executor evidence. Settlement swaps its durable pending slot
// for the mirror in one transaction; it never changes the append receipt.

// Pure reservation arithmetic, also exercised by the interleaving witness.
// Stop at the first non-fitting operation: later appends cannot steal slots
// from an already dispatched operation, including after a lost response.
fn federation_executor_reserved_prefix(
    count: u64, bytes: u64, payload_bytes: impl IntoIterator<Item = u64>,
) -> usize {
    let (mut count, mut bytes, mut reserved) = (count, bytes, 0);
    for payload in payload_bytes {
        if count >= FEDERATION_EXECUTOR_MAX_COUNT
            || bytes.saturating_add(payload) > FEDERATION_EXECUTOR_MAX_BYTES { break; }
        count += 1;
        bytes += payload;
        reserved += 1;
    }
    reserved
}

impl MailboxStore {
    fn reserve_exact_federation_executor_wake<G>(
        &self, request: &FederationExecutorWakeRequest, authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        let _writer = self.lock_writer("checking remote wake reservation")?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "checking remote wake reservation")?;
        let reserved = self.executor_reservations.lock().expect("executor reservation mutex poisoned").refresh(&transaction)?;
        let expected = serde_json::to_value(request)?;
        if !reserved.iter().any(|(_, row)| serde_json::to_value(row).ok().as_ref() == Some(&expected)) {
            return Err(mailbox_store_error(MailboxStoreErrorKind::Capacity,
                "remote wake pending; reason capacity or retired reservation"));
        }
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(())
    }

    fn federation_executor_wake_state(&self, message: &str) -> Result<String> {
        let connection = self.connection()?;
        let mut reservations = self.executor_reservations.lock().expect("executor reservation mutex poisoned");
        self.federation_executor_wake_state_on(&connection, message, &mut reservations)
    }

    fn federation_executor_wake_state_on(&self, connection: &rusqlite::Connection, message: &str,
        reservations: &mut FederationExecutorReservationCache) -> Result<String> {
        let state: Option<String> = connection.query_row(
            "SELECT state FROM federation_mailbox_wake_obligations WHERE message_id=?1",
            [message], |row| row.get(0)).optional()?;
        if state.as_deref() == Some("settled") {
            let mirror: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM federation_executor_operations WHERE operation_key=?1)",
                [format!("mailbox-wake:{message}")], |row| row.get(0))?;
            return Ok(if mirror { "remote receiver durably owns wake recovery" }
                else { "legacy settled wake; receiver state unknown" }.into());
        }
        if state.as_deref() != Some("pending") { return Ok("remote wake state unknown".into()); }
        let binding: String = connection.query_row(
            "SELECT target_binding_json FROM federation_mailbox_wake_obligations WHERE message_id=?1",
            [message], |row| row.get(0))?;
        let target: FederationMailboxBinding = match serde_json::from_str(&binding) {
            Ok(binding) => binding,
            Err(_) => return Ok("remote wake pending; reason unsupported".into()),
        };
        if reservations.stale(&target) {
            return Ok("remote wake pending; reason stale_authority".into());
        }
        let wake = connection.query_row(
            "SELECT mailbox_id,id,sequence,unread_depth_at_append,sender_session_id,sender_name,topic
             FROM mailbox_messages WHERE id=?1", [message], |row| Ok(FederationExecutorWake {
                mailbox_id: row.get(0)?, message_id: row.get(1)?, sequence: row.get(2)?,
                unread_count: row.get(3)?, sender_session_id: row.get(4)?, sender_name: row.get(5)?,
                topic: row.get(6)?,
            }))?.with_display_copies();
        let request = FederationExecutorWakeRequest { protocol_version: FEDERATION_PROTOCOL_VERSION,
            operation_key: format!("mailbox-wake:{message}"), target, wake };
        if request.operation(1).is_err() {
            return Ok("remote wake pending; reason unsupported".into());
        }
        let reserved = match reservations.refresh(connection) {
            Ok(reserved) => reserved,
            Err(error) if error.downcast_ref::<MailboxStoreError>().is_some_and(|error|
                error.kind == MailboxStoreErrorKind::Capacity) =>
                return Ok("remote wake pending; reservation scan incomplete".into()),
            Err(error) => return Err(error),
        };
        Ok(if reserved.iter().any(|(_, request)| request.wake.message_id == message) {
            "remote wake pending; awaiting matching durable receiver receipt"
        } else { "remote wake pending; reason capacity" }.into())
    }

    // The returned set is a durable reservation, not authority to send. The
    // caller revalidates its live link and exact binding before network I/O.
    fn reserve_federation_executor_wakes(&self) -> Result<Vec<FederationExecutorWakeRequest>> {
        let _writer = self.lock_writer("reserving remote executor ownership")?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "reserving remote executor ownership")?;
        let requests = self.executor_reservations.lock().expect("executor reservation mutex poisoned").refresh(&transaction)?
            .into_iter().map(|(_, request)| request).collect();
        transaction.commit()?;
        Ok(requests)
    }

    fn settle_federation_executor_mirror<G>(
        &self, request: &FederationExecutorWakeRequest, operation: &FederationExecutorOperation,
        receipt: &FederationExecutorReceipt, authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        // Validate the entire echoed immutable identity, not just its key.
        if receipt.protocol_version != FEDERATION_PROTOCOL_VERSION || !receipt.received
            || receipt.operation != *operation || receipt.payload_digest != operation.digest()
            || request.operation(operation.enrollment_generation).map_err(|_| federation_executor_conflict())? != *operation
        { return Err(federation_executor_conflict()); }
        let _writer = self.lock_writer("settling remote executor ownership")?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "settling remote executor ownership")?;
        let obligation: (String, String) = transaction.query_row(
            "SELECT target_binding_json,state FROM federation_mailbox_wake_obligations WHERE message_id=?1",
            [&request.wake.message_id], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if serde_json::from_str::<FederationMailboxBinding>(&obligation.0)? != request.target {
            return Err(federation_executor_conflict());
        }
        if obligation.1 == "settled" {
            // Legacy settled obligations have no receiver receipt. They are
            // never replayed or upgraded into invented ownership evidence.
            let existing = federation_executor_read(&transaction, &operation.central_host_id,
                &operation.operation_key)?.ok_or_else(federation_executor_conflict)?;
            if existing.operation != *operation { return Err(federation_executor_conflict()); }
        } else {
            // No quota recheck: this exact payload already owns a slot in the
            // immutable pending prefix. Settlement is count/byte neutral.
            let mirror = federation_executor_admit(&transaction, operation, false, &mut self.executor_reservations.lock().expect("executor reservation mutex poisoned"))?;
            if mirror.stage != "received" || mirror.effect_state != "not_started"
                || mirror.queue_entry_id.is_some() || mirror.delivery_attempt_id.is_some()
                || mirror.runtime_ack.is_some() || mirror.processed_evidence.is_some()
                || mirror.processed_evidence_kind.is_some() { return Err(federation_executor_conflict()); }
            if transaction.execute(
                "UPDATE federation_mailbox_wake_obligations SET state='settled' WHERE message_id=?1 AND state='pending'",
                [&request.wake.message_id],
            )? != 1 { return Err(federation_executor_conflict()); }
        }
        let _authority = authorize()?;
        #[cfg(test)]
        federation_executor_test_cut("beforeMirrorCommit")?;
        transaction.commit()?;
        #[cfg(test)]
        federation_executor_test_cut("afterMirrorCommit")?;
        Ok(())
    }
}

struct FederationExecutorMirrorAuthority<'a> {
    app: &'a AppState,
    operation: &'a FederationExecutorOperation,
    lease: &'a FederationLinkLease,
}

impl FederationExecutorMirrorAuthority<'_> {
    fn lock(&self) -> Result<FederationMailboxAdmission<'_>> {
        let link = self.lease.channel.state.lock().expect("federation link mutex poisoned");
        if link.incarnation.as_deref() != Some(self.lease.incarnation.as_str())
            || link.recorded_incarnation.as_deref() != Some(self.lease.incarnation.as_str())
        { return Err(federation_executor_conflict()); }
        let inner = self.app.inner.lock().expect("state mutex poisoned");
        let peer = self.app.refresh_federation_transport_snapshot_locked(&inner,
            &self.lease.channel.transport_basis(), &self.lease.incarnation)
            .map_err(|_| federation_executor_conflict())?;
        let binding = self.operation.mailbox_binding();
        if federation_mailbox_central_locked(&inner).ok().as_ref() != Some(&binding.central)
            || peer.enrollment_generation != self.operation.enrollment_generation
            || !federation_mailbox_remote_in_domain(&inner.federation, &inner.preferences.federation, &peer, &binding)
        { return Err(federation_executor_conflict()); }
        Ok(FederationMailboxAdmission { _inner: inner, _link: Some(link) })
    }
}
