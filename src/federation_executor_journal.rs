// Inert satellite operation storage. These APIs never enqueue, dispatch,
// acknowledge a mailbox page or execute a lifecycle action. A future caller
// must validate authenticated current authority and hold its returned guard
// through commit; captured identity alone is not authorization.
// All journal rows count towards capacity, including processed dedup rows.
// There is deliberately no eviction or pruning API in this increment.
// Until a runtime caller is wired, allow these inert APIs outside tests only.
#[cfg_attr(not(test), allow(dead_code))]
const FEDERATION_EXECUTOR_MAX_COUNT: u64 = 1000;
#[cfg_attr(not(test), allow(dead_code))]
const FEDERATION_EXECUTOR_MAX_BYTES: u64 = 64 * 1024 * 1024;
#[cfg_attr(not(test), allow(dead_code))]
const FEDERATION_EXECUTOR_MAX_PAYLOAD: usize = 256 * 1024;

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct FederationExecutorOperation {
    central_host_id: String,
    central_epoch: u64,
    operation_key: String,
    target_host_id: String,
    enrollment_generation: u64,
    session_id: String,
    creation_id: String,
    placement_generation: u64,
    command_kind: String,
    payload: String,
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationExecutorOperation {
    fn validate(&self) -> Result<()> {
        for (value, bound) in [
            (&self.central_host_id, 256), (&self.operation_key, 1024),
            (&self.target_host_id, 256), (&self.session_id, 256), (&self.creation_id, 256),
        ] {
            if value.trim().is_empty() || value.len() > bound || value.contains('\0') {
                return Err(mailbox_store_error(MailboxStoreErrorKind::Validation,
                    "executor operation identity is empty, oversized or invalid"));
            }
        }
        if [self.central_epoch, self.enrollment_generation, self.placement_generation]
            .iter().any(|value| *value == 0 || *value > i64::MAX as u64)
            || !matches!(self.command_kind.as_str(), "wake" | "start" | "stop")
            || self.payload.len() > FEDERATION_EXECUTOR_MAX_PAYLOAD
        {
            return Err(mailbox_store_error(MailboxStoreErrorKind::Validation,
                "executor operation authority, command or payload is invalid"));
        }
        Ok(())
    }

    fn digest(&self) -> String {
        sha256_hex(self.payload.as_bytes())
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct FederationExecutorRecord {
    operation: FederationExecutorOperation,
    stage: String,
    effect_state: String,
    queue_entry_id: Option<String>,
    delivery_attempt_id: Option<String>,
    runtime_ack: Option<String>,
    processed_evidence_kind: Option<String>,
    processed_evidence: Option<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
enum FederationExecutorEvidence {
    Queued { queue_entry_id: String, delivery_attempt_id: String },
    // The write-ahead attempt is unknown until independent positive evidence.
    EffectAttempt { delivery_attempt_id: String },
    RuntimeAccepted { delivery_attempt_id: String, acknowledgement: String },
    // Callers must supply already-validated central ACK evidence or a matching
    // command terminal outcome. This storage layer cannot validate either.
    Processed { kind: FederationExecutorProcessedKind, evidence: String },
}

#[cfg_attr(not(test), allow(dead_code))]
enum FederationExecutorProcessedKind {
    MailboxAck,
    CommandOutcome,
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_executor_conflict() -> anyhow::Error {
    mailbox_store_error(MailboxStoreErrorKind::Conflict,
        "executor operation key, captured authority or retained evidence conflicts")
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_executor_evidence_text(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 4096 || value.contains('\0') {
        return Err(mailbox_store_error(MailboxStoreErrorKind::Validation,
            "executor evidence is empty, oversized or invalid"));
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
fn federation_executor_read(
    connection: &rusqlite::Connection,
    central: &str,
    key: &str,
) -> Result<Option<FederationExecutorRecord>> {
    let raw = connection.query_row(
        "SELECT central_host_id,central_epoch,operation_key,target_host_id,
         enrollment_generation,session_id,creation_id,placement_generation,command_kind,
         payload,payload_digest,stage,effect_state,queue_entry_id,delivery_attempt_id,
         runtime_ack,processed_evidence_kind,processed_evidence
         FROM federation_executor_operations WHERE central_host_id=?1 AND operation_key=?2",
        rusqlite::params![central, key],
        |row| Ok((FederationExecutorRecord {
            operation: FederationExecutorOperation {
                central_host_id: row.get(0)?, central_epoch: row.get(1)?,
                operation_key: row.get(2)?, target_host_id: row.get(3)?,
                enrollment_generation: row.get(4)?, session_id: row.get(5)?,
                creation_id: row.get(6)?, placement_generation: row.get(7)?,
                command_kind: row.get(8)?, payload: row.get(9)?,
            },
            stage: row.get(11)?, effect_state: row.get(12)?,
            queue_entry_id: row.get(13)?, delivery_attempt_id: row.get(14)?,
            runtime_ack: row.get(15)?, processed_evidence_kind: row.get(16)?,
            processed_evidence: row.get(17)?,
        }, row.get::<_, String>(10)?)),
    ).optional()?;
    raw.map(|(record, digest)| {
        record.operation.validate()?;
        if record.operation.digest() != digest {
            bail!("executor journal payload does not match its retained digest");
        }
        Ok(record)
    }).transpose()
}

#[cfg_attr(not(test), allow(dead_code))]
impl FederationExecutorRecord {
    fn apply(&mut self, evidence: FederationExecutorEvidence) -> Result<()> {
        match evidence {
            FederationExecutorEvidence::Queued { queue_entry_id, delivery_attempt_id } => {
                federation_executor_evidence_text(&queue_entry_id)?;
                federation_executor_evidence_text(&delivery_attempt_id)?;
                if self.queue_entry_id.as_ref().is_some_and(|old| old != &queue_entry_id)
                    || self.delivery_attempt_id.as_ref().is_some_and(|old| old != &delivery_attempt_id)
                {
                    return Err(federation_executor_conflict());
                }
                self.queue_entry_id = Some(queue_entry_id);
                self.delivery_attempt_id = Some(delivery_attempt_id);
                if self.stage == "received" { self.stage = "queued".to_owned(); }
            }
            FederationExecutorEvidence::EffectAttempt { delivery_attempt_id } => {
                self.require_attempt(&delivery_attempt_id)?;
                if self.effect_state != "confirmed" { self.effect_state = "unknown".to_owned(); }
            }
            FederationExecutorEvidence::RuntimeAccepted { delivery_attempt_id, acknowledgement } => {
                self.require_attempt(&delivery_attempt_id)?;
                federation_executor_evidence_text(&acknowledgement)?;
                if self.runtime_ack.as_ref().is_some_and(|old| old != &acknowledgement) {
                    return Err(federation_executor_conflict());
                }
                if self.effect_state == "not_started" {
                    return Err(federation_executor_conflict());
                }
                self.runtime_ack = Some(acknowledgement);
                self.effect_state = "confirmed".to_owned();
                if self.stage != "processed" { self.stage = "runtime_accepted".to_owned(); }
            }
            FederationExecutorEvidence::Processed { kind, evidence } => {
                federation_executor_evidence_text(&evidence)?;
                let kind = match (self.operation.command_kind.as_str(), kind) {
                    ("wake", FederationExecutorProcessedKind::MailboxAck) => "mailbox_ack",
                    ("start" | "stop", FederationExecutorProcessedKind::CommandOutcome) => "command_outcome",
                    _ => return Err(federation_executor_conflict()),
                };
                if self.processed_evidence_kind.as_deref().is_some_and(|old| old != kind)
                    || self.processed_evidence.as_ref().is_some_and(|old| old != &evidence)
                {
                    return Err(federation_executor_conflict());
                }
                self.processed_evidence_kind = Some(kind.to_owned());
                self.processed_evidence = Some(evidence);
                self.stage = "processed".to_owned();
                // Processing evidence does not silently settle an uncertain
                // effect. Runtime/effect reconciliation is a separate act.
            }
        }
        Ok(())
    }

    fn require_attempt(&self, attempt: &str) -> Result<()> {
        federation_executor_evidence_text(attempt)?;
        if self.delivery_attempt_id.as_deref() != Some(attempt) || self.queue_entry_id.is_none() {
            return Err(federation_executor_conflict());
        }
        Ok(())
    }
}

impl MailboxStore {
    #[cfg_attr(not(test), allow(dead_code))]
    fn admit_federation_executor_operation<G>(
        &self,
        operation: &FederationExecutorOperation,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<FederationExecutorRecord> {
        operation.validate()?;
        let _writer = self.lock_writer("receiving executor operation")?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "receiving executor operation")?;
        if let Some(existing) = federation_executor_read(&transaction,
            &operation.central_host_id, &operation.operation_key)?
        {
            if existing.operation != *operation { return Err(federation_executor_conflict()); }
            let _authority = authorize()?;
            transaction.commit()?;
            return Ok(existing);
        }
        let (count, bytes): (u64, u64) = transaction.query_row(
            "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM federation_executor_operations",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if count >= FEDERATION_EXECUTOR_MAX_COUNT
            || bytes.saturating_add(operation.payload.len() as u64) > FEDERATION_EXECUTOR_MAX_BYTES
        {
            return Err(mailbox_store_error(MailboxStoreErrorKind::Capacity,
                "executor journal is full; retained operations are not evicted"));
        }
        transaction.execute(
            "INSERT INTO federation_executor_operations(central_host_id,operation_key,central_epoch,
             target_host_id,enrollment_generation,session_id,creation_id,placement_generation,
             command_kind,payload_digest,payload,payload_bytes,stage,effect_state)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'received','not_started')",
            rusqlite::params![operation.central_host_id,operation.operation_key,operation.central_epoch,
                operation.target_host_id,operation.enrollment_generation,operation.session_id,
                operation.creation_id,operation.placement_generation,operation.command_kind,
                operation.digest(),operation.payload,operation.payload.len() as u64],
        )?;
        let record = federation_executor_read(&transaction,
            &operation.central_host_id, &operation.operation_key)?
            .ok_or_else(|| anyhow!("persisted executor operation disappeared"))?;
        let _authority = authorize()?;
        #[cfg(test)]
        federation_executor_test_cut("beforeReceiveCommit")?;
        transaction.commit()?;
        #[cfg(test)]
        federation_executor_test_cut("afterReceiveCommit")?;
        Ok(record)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn record_federation_executor_evidence<G>(
        &self,
        operation: &FederationExecutorOperation,
        evidence: FederationExecutorEvidence,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<FederationExecutorRecord> {
        operation.validate()?;
        let _writer = self.lock_writer("retaining executor evidence")?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "retaining executor evidence")?;
        let mut record = federation_executor_read(&transaction,
            &operation.central_host_id, &operation.operation_key)?
            .ok_or_else(|| mailbox_store_error(MailboxStoreErrorKind::NotFound,
                "executor operation has not been durably received"))?;
        if record.operation != *operation { return Err(federation_executor_conflict()); }
        record.apply(evidence)?;
        transaction.execute(
            "UPDATE federation_executor_operations SET stage=?3,effect_state=?4,queue_entry_id=?5,
             delivery_attempt_id=?6,runtime_ack=?7,processed_evidence_kind=?8,processed_evidence=?9
             WHERE central_host_id=?1 AND operation_key=?2",
            rusqlite::params![operation.central_host_id,operation.operation_key,record.stage,
                record.effect_state,record.queue_entry_id,record.delivery_attempt_id,record.runtime_ack,
                record.processed_evidence_kind,record.processed_evidence],
        )?;
        let _authority = authorize()?;
        #[cfg(test)]
        federation_executor_test_cut("beforeEvidenceCommit")?;
        transaction.commit()?;
        #[cfg(test)]
        federation_executor_test_cut("afterEvidenceCommit")?;
        Ok(record)
    }

    // A bounded recovery read, not a replay instruction. Processed rows with
    // unknown effects remain visible. The caller decides reconciliation.
    #[cfg_attr(not(test), allow(dead_code))]
    fn pending_federation_executor_operations(
        &self, central_host_id: &str, after_key: &str, limit: usize,
    ) -> Result<Vec<FederationExecutorRecord>> {
        let connection = self.connection()?;
        let keys = connection.prepare(
            "SELECT operation_key FROM federation_executor_operations
             WHERE central_host_id=?1 AND operation_key>?2
             AND (stage<>'processed' OR effect_state='unknown') ORDER BY operation_key LIMIT ?3",
        )?.query_map(rusqlite::params![central_host_id,after_key,limit.clamp(1,100) as u64],
            |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        keys.into_iter().map(|key| federation_executor_read(&connection, central_host_id, &key)?
            .ok_or_else(|| anyhow!("executor operation disappeared during bounded read")))
            .collect()
    }
}

#[cfg(test)]
thread_local! {
    static FEDERATION_EXECUTOR_TEST_CUT: std::cell::RefCell<Option<&'static str>> =
        const { std::cell::RefCell::new(None) };
    static FEDERATION_EXECUTOR_TEST_CRASH: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn federation_executor_test_cut(phase: &str) -> Result<()> {
    FEDERATION_EXECUTOR_TEST_CUT.with(|slot| {
        let mut cut = slot.borrow_mut();
        if *cut == Some(phase) {
            *cut = None;
            if FEDERATION_EXECUTOR_TEST_CRASH.with(|crash| crash.get()) {
                // Isolated child only: bypass connection/transaction destructors.
                std::process::exit(73);
            }
            bail!("injected executor persistence cut: {phase}");
        }
        Ok(())
    })
}
