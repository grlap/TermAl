// Home-owned relay membership, atomic append/wake, and metadata-only sender pins.
// Does not choose a host, authorize a caller, mirror bodies, or send network traffic.

fn relay_store_error(message: &str) -> anyhow::Error {
    mailbox_store_error(MailboxStoreErrorKind::Conflict, message)
}

fn check_mailbox_relay_home(
    connection: &rusqlite::Connection,
    relay: &MailboxRelayAppend,
) -> Result<()> {
    relay
        .input()
        .map_err(|error| relay_store_error(&error.message))?;
    if relay.conversation.home.store_id != mailbox_relay_store_id(connection)? {
        return Err(relay_store_error(
            "mailbox relay home store changed; original request remains pinned",
        ));
    }
    Ok(())
}

fn commit_mailbox_relay_append(
    connection: &rusqlite::Connection,
    relay: &MailboxRelayAppend,
    peer: &FederationPeerSnapshot,
    mailbox_id: &str,
    message_id: &str,
    sequence: u64,
    unread_count: u64,
) -> Result<()> {
    check_mailbox_relay_home(connection, relay)?;
    let mut names = [String::new(), String::new()];
    for (index, principal) in relay.conversation.participants.iter().enumerate() {
        names[index] = if principal == &relay.sender {
            relay.sender_name.clone()
        } else {
            relay.target_name.clone()
        };
    }
    remember_mailbox_relay_association(connection, &relay.conversation, mailbox_id, &names)?;
    let wake = MailboxRelayWake {
        version: MAILBOX_RELAY_VERSION,
        conversation: relay.conversation.clone(),
        sender: relay.sender.clone(),
        target: relay.target.clone(),
        mailbox_id: mailbox_id.to_owned(),
        message_id: message_id.to_owned(),
        sequence,
        unread_count,
        sender_name: relay.sender_name.clone(),
        topic: relay.topic.clone(),
    };
    wake.validate()
        .map_err(|error| relay_store_error(&error.message))?;
    validate_mailbox_relay_wake_peer(&wake, peer, true)?;
    let existing: Option<String> = connection
        .query_row(
            "SELECT peer_json FROM mailbox_relay_wakes WHERE message_id=?1",
            [message_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(original) = &existing {
        let original: FederationPeerSnapshot = serde_json::from_str(original)?;
        if !same_mailbox_relay_enrollment(&original, peer) {
            return Err(relay_store_error(
                "native relay wake enrollment changed; original obligation remains held",
            ));
        }
    } else {
        let pending: u64 = connection.query_row(
            "SELECT count(*) FROM mailbox_relay_wakes WHERE delivered=0",
            [],
            |row| row.get(0),
        )?;
        if pending >= 4096 {
            return Err(mailbox_store_error(
                MailboxStoreErrorKind::Capacity,
                "native relay pending wake capacity reached",
            ));
        }
    }
    connection.execute(
        "INSERT INTO mailbox_relay_wakes(message_id,record_version,mailbox_id,principal_key,through_sequence,wake_json,peer_json)
         VALUES(?1,1,?2,?3,?4,?5,?6) ON CONFLICT(message_id) DO NOTHING",
        rusqlite::params![message_id, mailbox_id, relay.target.participant_key(), sequence, serde_json::to_string(&wake)?,serde_json::to_string(peer)?],
    )?;
    Ok(())
}

fn remember_mailbox_relay_association(
    connection: &rusqlite::Connection,
    conversation: &MailboxRelayConversation,
    mailbox_id: &str,
    names: &[String; 2],
) -> Result<()> {
    conversation
        .validate()
        .map_err(|error| relay_store_error(&error.message))?;
    if !valid_mailbox_relay_id(mailbox_id)
        || names.iter().any(|name| !valid_mailbox_relay_name(name))
    {
        return Err(relay_store_error("invalid relay association"));
    }
    let pair = conversation.pair_key();
    let prior: Option<(String, String)> = connection
        .query_row(
            "SELECT mailbox_id,conversation_json FROM mailbox_relay_associations WHERE pair_key=?1",
            [&pair],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((existing_mailbox, json)) = prior {
        let existing: MailboxRelayConversation = serde_json::from_str(&json)?;
        if existing_mailbox != mailbox_id || existing != *conversation {
            return Err(relay_store_error(
                "relay conversation already has a different immutable home association",
            ));
        }
        return Ok(());
    }
    connection.execute(
        "INSERT INTO mailbox_relay_associations(pair_key,record_version,mailbox_id,conversation_json,names_json) VALUES(?1,1,?2,?3,?4)",
        rusqlite::params![pair, mailbox_id,serde_json::to_string(conversation)?,serde_json::to_string(names)?],
    )?;
    Ok(())
}

impl MailboxStore {
    fn mailbox_relay_intent(
        &self,
        owner: &str,
        key: &str,
    ) -> Result<Option<(MailboxRelayConversation, FederationPeerSnapshot, String)>> {
        let connection = self.connection()?;
        let row:Option<(String,String,String)>=connection.query_row(
            "SELECT conversation_json,peer_json,requested_target FROM mailbox_relay_intents WHERE owner_session_id=?1 AND idempotency_key=?2",
            rusqlite::params![owner,key],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
        row.map(|(conversation, peer, target)| {
            Ok((
                serde_json::from_str(&conversation)?,
                serde_json::from_str(&peer)?,
                target,
            ))
        })
        .transpose()
    }

    fn mailbox_relay_conversations(
        &self,
        actor: &MailboxRelayPrincipal,
    ) -> Result<Vec<MailboxRelayConversation>> {
        let connection = self.connection()?;
        let mut statement=connection.prepare("SELECT conversation_json FROM mailbox_relay_associations UNION SELECT conversation_json FROM mailbox_relay_intents")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut conversations = Vec::new();
        for row in rows {
            let conversation: MailboxRelayConversation = serde_json::from_str(&row?)?;
            conversation
                .validate()
                .map_err(|error| relay_store_error(&error.message))?;
            if conversation.participants.contains(actor) {
                conversations.push(conversation);
            }
        }
        Ok(conversations)
    }

    fn append_mailbox_relay<G>(
        &self,
        relay: &MailboxRelayAppend,
        peer: &FederationPeerSnapshot,
        authorize: impl FnOnce() -> Result<G>,
        authorize_new: impl FnOnce(&G) -> Result<()>,
    ) -> Result<MailboxAppendReceipt> {
        let input = relay
            .input()
            .map_err(|error| relay_store_error(&error.message))?;
        // Current ownership fences apply to both branches. Readiness for a new
        // admission must not reinterpret an already committed exact retry.
        let appended = self.append_with_delivery_policy(
            &input, None, Some((relay, peer)), authorize, |_, authority| authorize_new(authority))?;
        let MailboxAppendResult {
            receipt,
            finalization,
        } = appended;
        drop(finalization);
        Ok(receipt)
    }

    fn mailbox_relay_association(
        &self,
        actor: &MailboxRelayPrincipal,
        mailbox_id: &str,
    ) -> Result<Option<MailboxRelayConversation>> {
        actor
            .validate()
            .map_err(|error| relay_store_error(&error.message))?;
        let connection = self.connection()?;
        let stored: Option<String> = connection
            .query_row(
                "SELECT conversation_json FROM mailbox_relay_associations WHERE mailbox_id=?1",
                [mailbox_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(json) = stored else {
            return Ok(None);
        };
        let conversation: MailboxRelayConversation = serde_json::from_str(&json)?;
        conversation.require_member(actor).map_err(|_| {
            mailbox_store_error(
                MailboxStoreErrorKind::NotFound,
                "native mailbox association not found",
            )
        })?;
        Ok(Some(conversation))
    }

    fn remember_mailbox_relay<G>(
        &self,
        conversation: &MailboxRelayConversation,
        mailbox_id: &str,
        names: &[String; 2],
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        let _writer =
            self.lock_writer_with_budget("retaining native relay home", Duration::from_secs(2))?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "retaining native relay home")?;
        remember_mailbox_relay_association(&transaction, conversation, mailbox_id, names)?;
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(())
    }

    fn pin_mailbox_relay<G>(
        &self,
        relay: &MailboxRelayAppend,
        peer: &FederationPeerSnapshot,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<Option<MailboxAppendReceipt>> {
        let digest = relay
            .digest()
            .map_err(|error| relay_store_error(&error.message))?;
        peer.validate()
            .map_err(|error| relay_store_error(&error.message))?;
        if peer.central != relay.conversation.home.host
            || relay.sender.host != peer.local
            || !relay
                .conversation
                .participants
                .iter()
                .any(|actor| actor.host == peer.local)
            || !relay
                .conversation
                .participants
                .iter()
                .any(|actor| actor.host == peer.peer)
        {
            return Err(relay_store_error(
                "relay intent peer does not match its native participants/home",
            ));
        }
        let _writer =
            self.lock_writer_with_budget("pinning native relay send", Duration::from_secs(2))?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "pinning native relay send")?;
        if relay.sender.store_id != mailbox_relay_store_id(&transaction)? {
            return Err(relay_store_error("native relay origin store changed"));
        }
        if mailbox_message_for_idempotency_key(
            &transaction,
            &relay.sender.native_session_id,
            &relay.idempotency_key,
        )?
        .is_some()
        {
            return Err(relay_store_error(
                "idempotency key belongs to an existing standalone mailbox send; no relay fallback",
            ));
        }
        if native_sender_has_domain_intent(&transaction, &relay.sender.native_session_id,
            &relay.sender.native_creation_id, &relay.idempotency_key)? {
            return Err(federation_delivery_conflict());
        }
        let sender = relay.sender.participant_key();
        let existing: Option<(String,String,String,Option<String>,String)> = transaction.query_row(
            "SELECT request_digest,conversation_json,peer_json,receipt_json,sender_key FROM mailbox_relay_intents WHERE owner_session_id=?1 AND idempotency_key=?2",
            rusqlite::params![relay.sender.native_session_id,relay.idempotency_key],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).optional()?;
        let receipt = if let Some((stored_digest, json, stored_peer, receipt, stored_sender)) =
            existing
        {
            let conversation: MailboxRelayConversation = serde_json::from_str(&json)?;
            let stored_peer: FederationPeerSnapshot = serde_json::from_str(&stored_peer)?;
            if stored_sender != sender
                || stored_digest != digest
                || conversation != relay.conversation
                || !same_mailbox_relay_enrollment(&stored_peer, peer)
            {
                return Err(relay_store_error(
                    "relay idempotency key is pinned to its original payload, native identities and home; reconcile rather than retarget",
                ));
            }
            receipt
                .map(|json| serde_json::from_str(&json))
                .transpose()?
        } else {
            let pending: u64 = transaction.query_row(
                "SELECT count(*) FROM mailbox_relay_intents WHERE receipt_json IS NULL",
                [],
                |row| row.get(0),
            )?;
            if pending >= 1024 {
                return Err(mailbox_store_error(
                    MailboxStoreErrorKind::Capacity,
                    "native relay pending intent capacity reached",
                ));
            }
            transaction.execute(
                "INSERT INTO mailbox_relay_intents(sender_key,owner_session_id,idempotency_key,record_version,request_digest,conversation_json,peer_json,created_at,requested_target)
                 VALUES(?1,?2,?3,1,?4,?5,?6,?7,?8)",
                rusqlite::params![sender,relay.sender.native_session_id,relay.idempotency_key,digest,serde_json::to_string(&relay.conversation)?,serde_json::to_string(peer)?,stamp_now(),relay.requested_target],
            )?;
            None
        };
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(receipt)
    }

    fn settle_mailbox_relay<G>(
        &self,
        relay: &MailboxRelayAppend,
        receipt: &MailboxAppendReceipt,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        if !valid_mailbox_relay_id(&receipt.mailbox_id)
            || receipt.sequence == 0
            || !receipt
                .message_id
                .strip_prefix("mailbox-message-")
                .is_some_and(mailbox_relay_uuid)
        {
            return Err(relay_store_error(
                "invalid native relay append receipt; outcome remains unknown",
            ));
        }
        let digest = relay
            .digest()
            .map_err(|error| relay_store_error(&error.message))?;
        let _writer =
            self.lock_writer_with_budget("retaining native relay outcome", Duration::from_secs(2))?;
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "retaining native relay outcome")?;
        let (stored,prior): (String,Option<String>) = transaction.query_row(
            "SELECT request_digest,receipt_json FROM mailbox_relay_intents WHERE sender_key=?1 AND idempotency_key=?2",
            rusqlite::params![relay.sender.participant_key(),relay.idempotency_key],|row|Ok((row.get(0)?,row.get(1)?)),
        )?;
        if stored != digest {
            return Err(relay_store_error(
                "native relay outcome does not match the pinned request",
            ));
        }
        if let Some(json) = prior {
            let prior: MailboxAppendReceipt = serde_json::from_str(&json)?;
            if prior.mailbox_id != receipt.mailbox_id
                || prior.message_id != receipt.message_id
                || prior.sequence != receipt.sequence
            {
                return Err(relay_store_error("native relay append outcome changed"));
            }
        } else {
            transaction.execute("UPDATE mailbox_relay_intents SET receipt_json=?3 WHERE sender_key=?1 AND idempotency_key=?2 AND receipt_json IS NULL",
                rusqlite::params![relay.sender.participant_key(),relay.idempotency_key,serde_json::to_string(receipt)?])?;
        }
        let names = relay.conversation.participants.clone().map(|principal| {
            if principal == relay.sender {
                relay.sender_name.clone()
            } else {
                relay.target_name.clone()
            }
        });
        remember_mailbox_relay_association(
            &transaction,
            &relay.conversation,
            &receipt.mailbox_id,
            &names,
        )?;
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(())
    }
}
