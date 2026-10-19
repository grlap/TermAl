// Owns the sender's durable retry registry and aggregate admission bounds.
// Does not serve mailbox content or infer canonical outcomes from absence.
// Every mutation uses the shared coordination writer before taking state locks.
const FEDERATION_INTENT_MAX_COUNT: u64 = 1000;
const FEDERATION_INTENT_MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
enum FederationIntentRefusal {
    CentralCapabilityUnsupported,
}

fn federation_delivery_unsupported() -> ApiError {
    ApiError::conflict("central does not support durable sender intents; this request made no delivery attempt")
}

fn federation_delivery_local_refusal() -> ApiError {
    ApiError::conflict("delivery locally refused: centralCapabilityUnsupported; terminal outcome retained")
}

struct FederationIntent {
    sender: String,
    key: String,
    owner: String,
    creation: String,
    target: String,
    digest: String,
    state: String,
    payload: Option<String>,
    receipt: Option<String>,
    identity_json: String,
}

impl FederationIntent {
    fn local_refusal(&self) -> Option<FederationIntentRefusal> {
        (self.state == "refused").then(|| self.receipt.as_deref()
            .and_then(|value| serde_json::from_str::<FederationIntentRefusal>(value).ok())).flatten()
    }

    fn terminal_error(&self) -> Option<ApiError> {
        if self.local_refusal().is_some() {
            return Some(federation_delivery_local_refusal());
        }
        matches!(self.state.as_str(), "refused" | "abandoned").then(|| {
            ApiError::conflict(format!("central delivery is {}; authoritative outcome retained", self.state))
        })
    }

    fn identity(&self) -> Result<FederationDeliveryIdentity> {
        let identity: FederationDeliveryIdentity = serde_json::from_str(&self.identity_json)?;
        if identity.version != 1
            || identity.sender.participant_key() != self.sender
            || identity.target.participant_key() != self.target
            || identity.key != self.key
        {
            return Err(federation_delivery_conflict());
        }
        Ok(identity)
    }
    fn status(&self) -> Value {
        let mut status = json!({"idempotencyKey":self.key,"targetSessionId":self.target,
            "canonicalDigest":self.digest,"state":self.state});
        // receipt_json is state-tagged: only homeCommitted decodes a receipt;
        // a sender-local refusal retains its typed cause across GC and reboot.
        if self.state == "refused" {
            if let Some(reason) = self.local_refusal() {
                status["refusalReason"] = json!(reason);
                status["refusalSource"] = json!("sender");
            }
        }
        status
    }

    fn matches_owner(&self, session: &str, creation: &str) -> Result<()> {
        if self.owner != session || self.creation != creation {
            return Err(federation_delivery_conflict());
        }
        Ok(())
    }

    fn committed_receipt(&self) -> Result<Option<MailboxAppendReceipt>> {
        if self.state != "homeCommitted" {
            return Ok(None);
        }
        let mut receipt: MailboxAppendReceipt = serde_json::from_str(
            self.receipt
                .as_deref()
                .ok_or_else(|| anyhow!("committed intent missing receipt"))?,
        )?;
        receipt.duplicate = true;
        receipt.sender_cursor_advanced = false;
        Ok(Some(receipt))
    }
}

fn federation_intent_read(
    connection: &rusqlite::Connection,
    sender: &str,
    key: &str,
) -> Result<Option<FederationIntent>> {
    Ok(connection
        .query_row(
            "SELECT sender_session_id,idempotency_key,owner_session_id,owner_creation_id,
         target_session_id,canonical_digest,state,payload,receipt_json,envelope_json
         FROM federation_mailbox_intents WHERE sender_session_id=?1 AND idempotency_key=?2",
            rusqlite::params![sender, key],
            |row| {
                Ok(FederationIntent {
                    sender: row.get(0)?,
                    key: row.get(1)?,
                    owner: row.get(2)?,
                    creation: row.get(3)?,
                    target: row.get(4)?,
                    digest: row.get(5)?,
                    state: row.get(6)?,
                    payload: row.get(7)?,
                    receipt: row.get(8)?,
                    identity_json: row.get(9)?,
                })
            },
        )
        .optional()?)
}

impl MailboxStore {
    fn refuse_unattempted_federation_intent<G>(
        &self,
        delivery: &FederationDelivery,
        owner: &str,
        creation: &str,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<Option<FederationIntentRefusal>> {
        let _writer = self.lock_writer("refusing an unsupported queued delivery")?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "refusing an unsupported queued delivery")?;
        let mut refusal = None;
        if let Some(intent) = federation_intent_read(&transaction, &delivery.sender.participant_key(), &delivery.key)? {
            intent.matches_owner(owner, creation)?;
            if intent.digest != delivery.digest() {
                return Err(federation_delivery_conflict());
            }
            // Dispatch is permitted only after a successful mark-unknown
            // transaction. This writer-serialized CAS cannot erase uncertainty.
            if intent.state == "localIntentDurable" {
                let changed = transaction.execute(
                    "UPDATE federation_mailbox_intents SET state='refused',receipt_json=?4
                     WHERE sender_session_id=?1 AND idempotency_key=?2 AND canonical_digest=?3
                     AND state='localIntentDurable'",
                    rusqlite::params![delivery.sender.participant_key(),delivery.key,delivery.digest(),
                        serde_json::to_string(&FederationIntentRefusal::CentralCapabilityUnsupported)?],
                )?;
                if changed != 1 { return Err(federation_delivery_conflict()); }
                refusal = Some(FederationIntentRefusal::CentralCapabilityUnsupported);
            } else {
                refusal = intent.local_refusal();
            }
        }
        #[cfg(test)]
        federation_delivery_test_cut("beforeLocalRefusal");
        let _current = authorize()?;
        transaction.commit()?;
        Ok(refusal)
    }

    fn prepare_federation_intent<G>(
        &self,
        delivery: &FederationDelivery,
        owner: &str,
        creation: &str,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<FederationIntent> {
        let _writer = self.lock_writer("persisting sender delivery intent")?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "persisting sender delivery intent")?;
        let sender = delivery.sender.participant_key();
        transaction.execute(
            "INSERT INTO federation_mailbox_retired_namespaces(sender_session_id,retired_at)
             SELECT DISTINCT sender_session_id,?3 FROM federation_mailbox_intents
             WHERE owner_session_id=?1 AND owner_creation_id<>?2
             ON CONFLICT(sender_session_id) DO NOTHING",
            rusqlite::params![owner, creation, stamp_now()],
        )?;
        let retired: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM federation_mailbox_retired_namespaces WHERE sender_session_id=?1)",
            [&sender], |row| row.get(0))?;
        if retired {
            return Err(federation_delivery_conflict());
        }
        if let Some(existing) = federation_intent_read(&transaction, &sender, &delivery.key)? {
            existing.matches_owner(owner, creation)?;
            if existing.digest != delivery.digest()
                || matches!(existing.state.as_str(), "refused" | "abandoned")
            {
                return Err(federation_delivery_conflict());
            }
            let _authority = authorize()?;
            transaction.commit()?;
            return Ok(existing);
        }
        let payload = serde_json::to_string(delivery)?;
        let bytes = payload.len() as u64;
        let (count, total): (u64, u64) = transaction.query_row(
            "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM federation_mailbox_intents
             WHERE state IN ('localIntentDurable','appendOutcomeUnknown')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if count >= FEDERATION_INTENT_MAX_COUNT
            || total.saturating_add(bytes) > FEDERATION_INTENT_MAX_BYTES
        {
            return Err(mailbox_store_error(
                MailboxStoreErrorKind::Capacity,
                "sender intent queue is full (1000 pending intents or 64 MiB); existing unknown outcomes are retained",
            ));
        }
        let envelope = serde_json::to_string(&json!({"version":delivery.version,
            "sender":delivery.sender,"target":delivery.target,"key":delivery.key}))?;
        transaction.execute(
            "INSERT INTO federation_mailbox_intents(sender_session_id,idempotency_key,sender_host_id,
             owner_session_id,owner_creation_id,target_session_id,canonical_digest,envelope_json,
             state,payload,payload_bytes) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'localIntentDurable',?9,?10)",
            rusqlite::params![sender,delivery.key,delivery.sender.identity.host_id,owner,creation,
                delivery.target.participant_key(),delivery.digest(),envelope,payload,bytes])?;
        let result = federation_intent_read(&transaction, &sender, &delivery.key)?
            .ok_or_else(|| anyhow!("persisted intent disappeared"))?;
        #[cfg(test)]
        federation_delivery_test_cut("beforeLocalDurability");
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(result)
    }

    fn mark_federation_intent_unknown<G>(
        &self,
        delivery: &FederationDelivery,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        let _writer = self.lock_writer("marking delivery attempt")?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "marking delivery attempt")?;
        let changed = transaction.execute(
            "UPDATE federation_mailbox_intents SET state='appendOutcomeUnknown'
             WHERE sender_session_id=?1 AND idempotency_key=?2 AND canonical_digest=?3
             AND state IN ('localIntentDurable','appendOutcomeUnknown')",
            rusqlite::params![
                delivery.sender.participant_key(),
                delivery.key,
                delivery.digest()
            ],
        )?;
        if changed != 1 {
            return Err(federation_delivery_conflict());
        }
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(())
    }

    fn settle_federation_intent(
        &self,
        delivery: &FederationDelivery,
        receipt: &MailboxAppendReceipt,
    ) -> Result<()> {
        let _writer = self.lock_writer("recording confirmed central append")?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "recording confirmed central append")?;
        let changed = transaction.execute(
            "UPDATE federation_mailbox_intents SET state='homeCommitted',message_id=?4,sequence=?5,receipt_json=?6
             WHERE sender_session_id=?1 AND idempotency_key=?2 AND canonical_digest=?3
             AND state IN ('localIntentDurable','appendOutcomeUnknown','homeCommitted')",
            rusqlite::params![delivery.sender.participant_key(),delivery.key,delivery.digest(),
                receipt.message_id,receipt.sequence,serde_json::to_string(receipt)?])?;
        if changed != 1 {
            return Err(federation_delivery_conflict());
        }
        transaction.commit()?;
        Ok(())
    }

    fn settle_federation_terminal_intent<G>(
        &self,
        delivery: &FederationDelivery,
        state: FederationDeliveryTerminalState,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        let _writer = self.lock_writer("retaining authoritative terminal delivery outcome")?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "retaining authoritative terminal delivery outcome")?;
        let changed = transaction.execute(
            "UPDATE federation_mailbox_intents SET state=?4 WHERE sender_session_id=?1
             AND idempotency_key=?2 AND canonical_digest=?3
             AND state IN ('localIntentDurable','appendOutcomeUnknown',?4)",
            rusqlite::params![delivery.sender.participant_key(),delivery.key,delivery.digest(),state.as_str()])?;
        if changed != 1 {
            return Err(federation_delivery_conflict());
        }
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(())
    }

    fn retire_federation_intent_owner(&self, owner: &str, creation: &str) -> Result<()> {
        if self.connection_if_enabled().is_none() {
            return Ok(());
        }
        let _writer = self.lock_writer("retiring sender namespace")?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "retiring sender namespace")?;
        transaction.execute(
            "INSERT INTO federation_mailbox_retired_namespaces(sender_session_id,retired_at)
             SELECT DISTINCT sender_session_id,?3 FROM federation_mailbox_intents
             WHERE owner_session_id=?1 AND owner_creation_id=?2
             ON CONFLICT(sender_session_id) DO NOTHING",
            rusqlite::params![owner, creation, stamp_now()],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn federation_delivery_settlement_error(error: anyhow::Error) -> ApiError {
    let mut error = mailbox_api_error(error);
    error.message = "central append committed, but local outcome retention failed; retry the exact delivery to recover the original receipt".to_owned();
    error
}
