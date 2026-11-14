// Immutable intent/admission data under the existing executor storage owner.
// No lifecycle, provider attempt, authorization snapshot or runnable instruction
// is restored from these rows. Runtime facts come from primary PromptDelivery.
// Recovered selectively from the retained direct-prompt journal component.
// These bounds exercise the unmounted foundation, not a production retention
// policy. Production needs compact identity owned by the real delivery path.
const REMOTE_PROMPT_FOUNDATION_SENDER_COUNT: u64 = 1000;
const REMOTE_PROMPT_FOUNDATION_RECEIVER_COUNT: u64 = 65_536;
const REMOTE_PROMPT_FOUNDATION_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
enum RemotePromptJournalWriteMode {
    Production,
    #[cfg(test)]
    Fixture,
}

impl RemotePromptJournalWriteMode {
    fn ensure_supported(self) -> Result<()> {
        match self {
            Self::Production => Err(mailbox_store_error(
                MailboxStoreErrorKind::Validation,
                "direct-prompt production writes unavailable until compact retention is integrated with the delivery owner",
            )),
            #[cfg(test)]
            Self::Fixture => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum RemotePromptJournalSide {
    Sender,
    Receiver,
}

#[cfg_attr(not(test), allow(dead_code))]
impl RemotePromptJournalSide {
    fn table(self) -> &'static str {
        match self {
            Self::Sender => "remote_prompt_intents",
            Self::Receiver => "remote_prompt_receives",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
struct RemotePromptJournalRecord {
    operation_id: Uuid,
    side: RemotePromptJournalSide,
    intent: remote_prompt_authority::Intent,
    evidence: RemotePromptJournalEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct RemotePromptQueueAcknowledgement {
    #[serde(deserialize_with = "remote_prompt_projection_uuid")]
    entry_id: Uuid,
    #[serde(deserialize_with = "remote_prompt_projection_uuid")]
    write_ahead: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(not(test), allow(dead_code))]
struct RemotePromptJournalEvidence {
    schema_version: u8,
    // Source write-ahead before any possible network entry. Never reset by a
    // timeout, reconnect, absent status or duplicate. Not a send instruction.
    may_have_entered: bool,
    // Projection only, recorded after the exact primary queue-content fence.
    // This is not a handoff/attempt or permission to recreate a missing queue.
    #[serde(deserialize_with = "remote_prompt_projection_optional")]
    queue: Option<RemotePromptQueueAcknowledgement>,
}

#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
struct RemotePromptJournalAdmission {
    record: RemotePromptJournalRecord,
    // Only the actual first receive may capture same-boot scheduling intent.
    // Duplicates, reloads and read-only reconciliation do not own activation.
    inserted: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_invalid_record() -> anyhow::Error {
    mailbox_store_error(
        MailboxStoreErrorKind::Validation,
        "invalid or unsupported retained direct-prompt record",
    )
}

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_journal_conflict() -> anyhow::Error {
    mailbox_store_error(
        MailboxStoreErrorKind::Conflict,
        "direct-prompt identity or retained evidence conflicts",
    )
}

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_stable_id(domain: &[u8], fields: &[&[u8]]) -> Uuid {
    let mut hash = Sha256::new();
    for value in std::iter::once(domain).chain(fields.iter().copied()) {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value);
    }
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80; // application-defined, domain-separated UUIDv8
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_operation_id(intent: &remote_prompt_authority::Intent) -> Uuid {
    let (basis, _, key, _) = intent.components();
    remote_prompt_operation_id_for_source(&basis.source, key)
}

fn remote_prompt_operation_id_for_source(
    source: &remote_prompt_authority::NativeRoot,
    key: &str,
) -> Uuid {
    let (host, epoch, session, creation) = source.components();
    remote_prompt_stable_id(
        b"termal.direct-prompt.operation.v1",
        &[
            host.as_bytes(),
            &epoch.to_be_bytes(),
            session.as_bytes(),
            creation.as_bytes(),
            key.as_bytes(),
        ],
    )
}

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_intent_digest(intent: &remote_prompt_authority::Intent) -> String {
    intent
        .digest()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn remote_prompt_queue_id(operation: Uuid) -> Uuid {
    remote_prompt_stable_id(b"termal.direct-prompt.queue.v1", &[operation.as_bytes()])
}

#[cfg_attr(not(test), allow(dead_code))]
impl RemotePromptJournalRecord {
    fn prompt_id(&self) -> Uuid {
        remote_prompt_queue_id(self.operation_id)
    }

    fn evidence_json(&self) -> Result<String> {
        let bytes =
            serde_json::to_string(&self.evidence).map_err(|_| remote_prompt_invalid_record())?;
        if bytes.len() > remote_prompt_protocol::RESPONSE_BYTES {
            return Err(remote_prompt_invalid_record());
        }
        Ok(bytes)
    }

    fn new(side: RemotePromptJournalSide, intent: remote_prompt_authority::Intent) -> Self {
        Self {
            operation_id: remote_prompt_operation_id(&intent),
            side,
            intent,
            evidence: RemotePromptJournalEvidence {
                schema_version: 1,
                may_have_entered: side == RemotePromptJournalSide::Receiver,
                queue: None,
            },
        }
    }

    fn validate(&self) -> Result<()> {
        if self.operation_id != remote_prompt_operation_id(&self.intent)
            || self.evidence.schema_version != 1
            || (self.side == RemotePromptJournalSide::Sender && self.evidence.queue.is_some())
            || (self.side == RemotePromptJournalSide::Receiver && !self.evidence.may_have_entered)
            || self.evidence.queue.as_ref().is_some_and(|queue| {
                queue.entry_id != self.prompt_id() || queue.write_ahead.is_nil()
            })
        {
            return Err(remote_prompt_invalid_record());
        }
        self.evidence_json()?;
        Ok(())
    }

    fn validate_successor(&self, next: &Self) -> Result<()> {
        next.validate()?;
        if self.operation_id != next.operation_id
            || self.side != next.side
            || self.intent != next.intent
            || (self.evidence.may_have_entered && !next.evidence.may_have_entered)
            || (self.evidence.queue.is_some() && self.evidence.queue != next.evidence.queue)
        {
            return Err(remote_prompt_journal_conflict());
        }
        Ok(())
    }
}

// Charge all serialized retained columns, with bounded future evidence already
// reserved at admission. This is not a claim about SQLite page/OS overhead.
#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_retained_bytes(
    intent: &remote_prompt_authority::Intent,
    encoded: &str,
    evidence: &str,
) -> u64 {
    let (basis, _, key, _) = intent.components();
    let (_, _, session, _) = basis.source.components();
    (36 + 8 + session.len() + 36 + key.len() + 36 + encoded.len() + 64 + evidence.len()) as u64
}

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_retained_usage(
    connection: &rusqlite::Connection,
    side: RemotePromptJournalSide,
) -> Result<(u64, u64)> {
    let invalid: bool = connection.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {} WHERE length(CAST(evidence_json AS BLOB))>?1)",
            side.table()
        ),
        [remote_prompt_protocol::RESPONSE_BYTES as u64],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(remote_prompt_invalid_record());
    }
    Ok(connection.query_row(&format!(
        "SELECT COUNT(*),COALESCE(SUM(retained_bytes + ?1 - length(CAST(evidence_json AS BLOB))),0) FROM {}", side.table()),
        [remote_prompt_protocol::RESPONSE_BYTES as u64], |row| Ok((row.get(0)?,row.get(1)?)))?)
}

// Boot admission validates retained data without creating permission, scheduling
// a receive or reconstructing a primary queue. Read one bounded row at a time.
#[cfg_attr(not(test), allow(dead_code))]
fn validate_remote_prompt_journals(connection: &rusqlite::Connection) -> Result<()> {
    for side in [
        RemotePromptJournalSide::Sender,
        RemotePromptJournalSide::Receiver,
    ] {
        let invalid: bool = connection.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {} WHERE length(CAST(intent_json AS BLOB))>?1
             OR length(CAST(evidence_json AS BLOB))>?2)",
                side.table()
            ),
            rusqlite::params![
                remote_prompt_protocol::REQUEST_BYTES,
                remote_prompt_protocol::RESPONSE_BYTES
            ],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(remote_prompt_invalid_record());
        }
        let mut statement = connection.prepare(&format!(
            "SELECT source_host_id,source_epoch,source_session_id,source_creation_id,idempotency_key FROM {}",
            side.table()))?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (host, epoch, session, creation, key) = row?;
            let canonical = |value: &str| {
                Uuid::parse_str(value)
                    .ok()
                    .filter(|id| !id.is_nil() && id.to_string() == value)
                    .ok_or_else(remote_prompt_invalid_record)
            };
            let source = remote_prompt_authority::NativeRoot::new(
                canonical(&host)?,
                epoch,
                session,
                canonical(&creation)?,
            )
            .map_err(|_| remote_prompt_invalid_record())?;
            remote_prompt_read_row(connection, side, &source, &key)?
                .ok_or_else(remote_prompt_invalid_record)?;
        }
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
fn remote_prompt_read_row(
    connection: &rusqlite::Connection,
    side: RemotePromptJournalSide,
    source: &remote_prompt_authority::NativeRoot,
    key: &str,
) -> Result<Option<RemotePromptJournalRecord>> {
    let (host, epoch, session, creation) = source.components();
    if epoch > i64::MAX as u64 {
        return Err(remote_prompt_invalid_record());
    }
    let row = connection.query_row(&format!(
        "SELECT operation_id,intent_json,intent_digest,evidence_json,retained_bytes FROM {}
         WHERE source_host_id=?1 AND source_epoch=?2 AND source_session_id=?3 AND source_creation_id=?4 AND idempotency_key=?5", side.table()),
        rusqlite::params![host.to_string(),epoch,session,creation.to_string(),key],
        |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,u64>(4)?))).optional()?;
    row.map(|(id, encoded, digest, evidence, bytes)| {
        if evidence.len() > remote_prompt_protocol::RESPONSE_BYTES {
            return Err(remote_prompt_invalid_record());
        }
        let intent = remote_prompt_protocol::decode_stored_intent(encoded.as_bytes())
            .map_err(|_| remote_prompt_invalid_record())?;
        let (basis, _, stored_key, _) = intent.components();
        if &basis.source != source
            || stored_key != key
            || digest != remote_prompt_intent_digest(&intent)
            || bytes != remote_prompt_retained_bytes(&intent, &encoded, &evidence)
        {
            return Err(remote_prompt_invalid_record());
        }
        let state: RemotePromptJournalEvidence =
            serde_json::from_str(&evidence).map_err(|_| remote_prompt_invalid_record())?;
        let record = RemotePromptJournalRecord {
            operation_id: Uuid::parse_str(&id).map_err(|_| remote_prompt_invalid_record())?,
            side,
            intent,
            evidence: state,
        };
        if record.operation_id.to_string() != id {
            return Err(remote_prompt_invalid_record());
        }
        record.validate()?;
        Ok(record)
    })
    .transpose()
}

#[cfg_attr(not(test), allow(dead_code))]
impl MailboxStore {
    fn admit_remote_prompt_journal<G>(
        &self,
        mode: RemotePromptJournalWriteMode,
        side: RemotePromptJournalSide,
        intent: &remote_prompt_authority::Intent,
        authorize: impl FnOnce(bool) -> Result<G>,
    ) -> Result<RemotePromptJournalAdmission> {
        self.admit_remote_prompt_journal_while_running(
            mode,
            side,
            intent,
            || Ok(()),
            authorize,
            |_, _| {},
        )
    }

    fn admit_remote_prompt_journal_while_running<G>(
        &self,
        mode: RemotePromptJournalWriteMode,
        side: RemotePromptJournalSide,
        intent: &remote_prompt_authority::Intent,
        before_writer: impl Fn() -> Result<()>,
        authorize: impl FnOnce(bool) -> Result<G>,
        committed: impl FnOnce(&mut G, &RemotePromptJournalRecord),
    ) -> Result<RemotePromptJournalAdmission> {
        mode.ensure_supported()?;
        let encoded = remote_prompt_protocol::encode_stored_intent(intent)
            .map_err(|_| remote_prompt_invalid_record())?;
        let encoded = String::from_utf8(encoded).map_err(|_| remote_prompt_invalid_record())?;
        let (basis, _, key, _) = intent.components();
        let (host, epoch, session, creation) = basis.source.components();
        if epoch > i64::MAX as u64 {
            return Err(remote_prompt_invalid_record());
        }
        before_writer()?;
        let _writer = self.lock_writer("retaining direct-prompt operation")?;
        // Stop may win while writer admission waits. Once authorized, the
        // owned receive job finishes COMMIT and capture even if HTTP cancels.
        before_writer()?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "retaining direct-prompt operation")?;
        if let Some(existing) = remote_prompt_read_row(&transaction, side, &basis.source, key)? {
            if existing.intent != *intent {
                return Err(remote_prompt_journal_conflict());
            }
            // The callback must check surviving original-owner read authority,
            // not demand a fresh grant or schedule a duplicate.
            let _current = authorize(false)?;
            transaction.commit()?;
            return Ok(RemotePromptJournalAdmission {
                record: existing,
                inserted: false,
            });
        }
        let record = RemotePromptJournalRecord::new(side, intent.clone());
        record.validate()?;
        let evidence = record.evidence_json()?;
        let retained = remote_prompt_retained_bytes(intent, &encoded, &evidence);
        let (count, bytes) = remote_prompt_retained_usage(&transaction, side)?;
        let limit_count = match side {
            RemotePromptJournalSide::Sender => REMOTE_PROMPT_FOUNDATION_SENDER_COUNT,
            RemotePromptJournalSide::Receiver => REMOTE_PROMPT_FOUNDATION_RECEIVER_COUNT,
        };
        let limit_bytes = REMOTE_PROMPT_FOUNDATION_BYTES;
        let reservation = retained
            .saturating_add((remote_prompt_protocol::RESPONSE_BYTES - evidence.len()) as u64);
        if count >= limit_count || bytes.saturating_add(reservation) > limit_bytes {
            return Err(mailbox_store_error(
                MailboxStoreErrorKind::Capacity,
                "direct-prompt foundation journal is full; production writes remain disabled",
            ));
        }
        transaction.execute(
            &format!(
                "INSERT INTO {} VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                side.table()
            ),
            rusqlite::params![
                host.to_string(),
                epoch,
                session,
                creation.to_string(),
                key,
                record.operation_id.to_string(),
                encoded,
                remote_prompt_intent_digest(intent),
                evidence,
                retained
            ],
        )?;
        // Current source/receiver policy is checked by the existing service
        // under its link -> state guard, held through this actual commit.
        let mut current = authorize(true)?;
        #[cfg(test)]
        federation_executor_test_cut("beforePromptJournalCommit")?;
        transaction.commit()?;
        // Capture only the first committed row, while the caller's existing
        // link/state/work guards are still held. The callback performs no I/O
        // or further locking. Duplicates above never acquire live scheduling.
        committed(&mut current, &record);
        #[cfg(test)]
        federation_executor_test_cut("afterPromptJournalCommit")?;
        Ok(RemotePromptJournalAdmission {
            record,
            inserted: true,
        })
    }

    fn read_remote_prompt_journal<G>(
        &self,
        side: RemotePromptJournalSide,
        source: &remote_prompt_authority::NativeRoot,
        key: &str,
        authorize_read: impl FnOnce() -> Result<G>,
    ) -> Result<Option<RemotePromptJournalRecord>> {
        let connection = self.connection()?;
        let record = remote_prompt_read_row(&connection, side, source, key)?;
        let _owner = authorize_read()?;
        Ok(record)
    }

    // CAS of admission/projection evidence only; no provider/lifecycle owner.
    fn mark_remote_prompt_possible_entry<G>(
        &self,
        mode: RemotePromptJournalWriteMode,
        expected: &RemotePromptJournalRecord,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<RemotePromptJournalRecord> {
        if expected.side != RemotePromptJournalSide::Sender {
            return Err(remote_prompt_journal_conflict());
        }
        let mut evidence = expected.evidence.clone();
        evidence.may_have_entered = true;
        self.record_remote_prompt_journal_evidence(mode, expected, evidence, authorize)
    }

    // The queue owner adapter is added with the actual primary queue seam.
    fn record_remote_prompt_journal_evidence<G>(
        &self,
        mode: RemotePromptJournalWriteMode,
        expected: &RemotePromptJournalRecord,
        evidence: RemotePromptJournalEvidence,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<RemotePromptJournalRecord> {
        mode.ensure_supported()?;
        expected.validate()?;
        let mut next = expected.clone();
        next.evidence = evidence;
        expected.validate_successor(&next)?;
        let side = expected.side;
        let (basis, _, key, _) = expected.intent.components();
        let _writer = self.lock_writer("retaining direct-prompt evidence")?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "retaining direct-prompt evidence")?;
        let current = remote_prompt_read_row(&transaction, side, &basis.source, key)?
            .ok_or_else(remote_prompt_journal_conflict)?;
        if current != *expected && current != next {
            return Err(remote_prompt_journal_conflict());
        }
        if current != next {
            let encoded = remote_prompt_protocol::encode_stored_intent(&next.intent)
                .map_err(|_| remote_prompt_invalid_record())?;
            let encoded = String::from_utf8(encoded).map_err(|_| remote_prompt_invalid_record())?;
            let evidence = next.evidence_json()?;
            let bytes = remote_prompt_retained_bytes(&next.intent, &encoded, &evidence);
            let changed = transaction.execute(
                &format!(
                    "UPDATE {} SET evidence_json=?2,retained_bytes=?3 WHERE operation_id=?1",
                    side.table()
                ),
                rusqlite::params![next.operation_id.to_string(), evidence, bytes],
            )?;
            if changed != 1 {
                return Err(remote_prompt_journal_conflict());
            }
        }
        let _current = authorize()?;
        transaction.commit()?;
        Ok(next)
    }
}
