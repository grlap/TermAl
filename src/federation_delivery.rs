// Owns immutable delivery identity and canonical digest version 1.
// Does not resolve aliases, grant authority, or read mailbox content.
// Fixed typed serialization distinguishes absent metadata from empty strings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationDelivery {
    version: u16,
    sender: FederationMailboxBinding,
    target: FederationMailboxBinding,
    key: String,
    message: String,
    topic: Option<String>,
    state_stamp: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationDeliveryIdentity {
    version: u16,
    sender: FederationMailboxBinding,
    target: FederationMailboxBinding,
    key: String,
}

impl FederationDelivery {
    fn digest(&self) -> String {
        sha256_hex(&serde_json::to_vec(self).expect("typed delivery serializes"))
    }

    fn input(&self, sender_name: String, target_name: String) -> MailboxAppendInput {
        MailboxAppendInput {
            sender_session_id: self.sender.participant_key(),
            target_session_id: self.target.participant_key(),
            sender_name,
            target_name,
            body: self.message.clone(),
            idempotency_key: self.key.clone(),
            topic: self.topic.clone(),
            state_stamp: self.state_stamp.clone(),
        }
    }

    fn validate(&self) -> Result<(), ApiError> {
        self.sender.validate()?;
        self.target.validate()?;
        if self.version != 1 || self.sender.central != self.target.central {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        validate_mailbox_append_input(&self.input(String::new(), String::new()))
            .map_err(mailbox_api_error)
    }
}

#[cfg(test)]
thread_local! {
    static FEDERATION_DELIVERY_TEST_CUT: std::cell::RefCell<Option<(&'static str, Box<dyn FnOnce()>)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn federation_delivery_test_cut(phase: &str) {
    FEDERATION_DELIVERY_TEST_CUT.with(|cell| {
        let hook = {
            let mut slot = cell.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|(expected, _)| *expected == phase)
            {
                slot.take()
            } else {
                None
            }
        };
        if let Some((_, hook)) = hook {
            hook();
        }
    });
}

fn federation_delivery_conflict() -> anyhow::Error {
    mailbox_store_error(
        MailboxStoreErrorKind::Conflict,
        "delivery key, captured target or sender namespace conflicts with its retained outcome",
    )
}

fn federation_delivery_check_central(
    connection: &rusqlite::Connection,
    delivery: &FederationDelivery,
) -> Result<()> {
    let sender = delivery.sender.participant_key();
    let retired: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM federation_mailbox_retired_namespaces WHERE sender_session_id=?1)",
        [&sender], |row| row.get(0))?;
    if retired {
        return Err(federation_delivery_conflict());
    }
    let existing: Option<(String, String)> = connection
        .query_row(
            "SELECT canonical_digest,state FROM federation_mailbox_dedup
         WHERE sender_host_id=?1 AND sender_session_id=?2 AND idempotency_key=?3",
            rusqlite::params![delivery.sender.identity.host_id, sender, delivery.key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if existing
        .is_some_and(|(digest, state)| digest != delivery.digest() || state != "homeCommitted")
    {
        return Err(federation_delivery_conflict());
    }
    Ok(())
}

fn federation_delivery_commit_central(
    transaction: &rusqlite::Transaction<'_>,
    delivery: &FederationDelivery,
    message_id: &str,
    sequence: u64,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO federation_mailbox_dedup(sender_host_id,sender_session_id,idempotency_key,
         target_session_id,canonical_digest,state,message_id,sequence)
         VALUES(?1,?2,?3,?4,?5,'homeCommitted',?6,?7)
         ON CONFLICT(sender_host_id,sender_session_id,idempotency_key) DO NOTHING",
        rusqlite::params![
            delivery.sender.identity.host_id,
            delivery.sender.participant_key(),
            delivery.key,
            delivery.target.participant_key(),
            delivery.digest(),
            message_id,
            sequence
        ],
    )?;
    transaction.execute(
        "INSERT INTO federation_mailbox_wake_obligations(message_id,target_session_id,target_binding_json,state)
         VALUES(?1,?2,?3,'pending') ON CONFLICT(message_id) DO NOTHING",
        rusqlite::params![message_id,delivery.target.participant_key(),serde_json::to_string(&delivery.target)?])?;
    Ok(())
}

impl MailboxStore {
    fn refuse_federation_delivery<G>(
        &self,
        delivery: &FederationDelivery,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        let _writer = self.lock_writer("recording central delivery refusal")?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "recording central delivery refusal")?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT canonical_digest,state FROM federation_mailbox_dedup
             WHERE sender_host_id=?1 AND sender_session_id=?2 AND idempotency_key=?3",
                rusqlite::params![
                    delivery.sender.identity.host_id,
                    delivery.sender.participant_key(),
                    delivery.key
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if existing.is_some_and(|(digest, state)| digest != delivery.digest() || state != "refused")
        {
            return Err(federation_delivery_conflict());
        }
        transaction.execute(
            "INSERT INTO federation_mailbox_dedup(sender_host_id,sender_session_id,idempotency_key,
             target_session_id,canonical_digest,state) VALUES(?1,?2,?3,?4,?5,'refused')
             ON CONFLICT(sender_host_id,sender_session_id,idempotency_key) DO NOTHING",
            rusqlite::params![
                delivery.sender.identity.host_id,
                delivery.sender.participant_key(),
                delivery.key,
                delivery.target.participant_key(),
                delivery.digest()
            ],
        )?;
        #[cfg(test)]
        federation_delivery_test_cut("beforeCentralRefusalCommit");
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(())
    }

    fn cancel_federation_delivery<G>(
        &self,
        delivery: &FederationDelivery,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        let _writer = self.lock_writer("serializing central delivery cancellation")?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "serializing central delivery cancellation")?;
        let sender = delivery.sender.participant_key();
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT canonical_digest,state FROM federation_mailbox_dedup
             WHERE sender_host_id=?1 AND sender_session_id=?2 AND idempotency_key=?3",
                rusqlite::params![delivery.sender.identity.host_id, sender, delivery.key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if existing
            .is_some_and(|(digest, state)| digest != delivery.digest() || state != "abandoned")
        {
            return Err(federation_delivery_conflict());
        }
        transaction.execute(
            "INSERT INTO federation_mailbox_dedup(sender_host_id,sender_session_id,idempotency_key,
             target_session_id,canonical_digest,state) VALUES(?1,?2,?3,?4,?5,'abandoned')
             ON CONFLICT(sender_host_id,sender_session_id,idempotency_key) DO NOTHING",
            rusqlite::params![
                delivery.sender.identity.host_id,
                sender,
                delivery.key,
                delivery.target.participant_key(),
                delivery.digest()
            ],
        )?;
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(())
    }
}
