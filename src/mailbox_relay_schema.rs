// Additive coordination storage for native relay ownership, not message replicas.
// Historical coordination schemas remain frozen; only this schema owns relay rows.

const MAILBOX_RELAY_SCHEMA_SQL: &str = "
    CREATE TABLE mailbox_relay_store (
      singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
      record_version INTEGER NOT NULL CHECK (record_version = 1),
      store_id TEXT NOT NULL UNIQUE
    );
    CREATE TABLE mailbox_relay_associations (
      pair_key TEXT PRIMARY KEY,
      record_version INTEGER NOT NULL CHECK (record_version = 1),
      mailbox_id TEXT NOT NULL UNIQUE,
      conversation_json TEXT NOT NULL,
      names_json TEXT NOT NULL,
      acknowledged_through INTEGER NOT NULL DEFAULT 0 CHECK (acknowledged_through >= 0)
    );
    CREATE TABLE mailbox_relay_intents (
      sender_key TEXT NOT NULL,
      owner_session_id TEXT NOT NULL,
      idempotency_key TEXT NOT NULL,
      record_version INTEGER NOT NULL CHECK (record_version = 1),
      request_digest TEXT NOT NULL,
      requested_target TEXT NOT NULL,
      conversation_json TEXT NOT NULL,
      peer_json TEXT NOT NULL,
      receipt_json TEXT,
      created_at TEXT NOT NULL,
      PRIMARY KEY (sender_key, idempotency_key),
      UNIQUE (owner_session_id, idempotency_key)
    );
    CREATE TABLE mailbox_relay_wakes (
      message_id TEXT PRIMARY KEY,
      record_version INTEGER NOT NULL CHECK (record_version = 1),
      mailbox_id TEXT NOT NULL,
      principal_key TEXT NOT NULL,
      through_sequence INTEGER NOT NULL CHECK (through_sequence > 0),
      wake_json TEXT NOT NULL,
      peer_json TEXT NOT NULL,
      delivered INTEGER NOT NULL DEFAULT 0 CHECK (delivered IN (0, 1)),
      FOREIGN KEY (message_id) REFERENCES mailbox_messages(id) ON DELETE CASCADE
    );
    CREATE INDEX mailbox_relay_wakes_pending ON mailbox_relay_wakes(delivered, message_id);
    CREATE TABLE mailbox_relay_notifications (
      mailbox_id TEXT NOT NULL,
      target_key TEXT NOT NULL,
      through_sequence INTEGER NOT NULL CHECK (through_sequence > 0),
      record_version INTEGER NOT NULL CHECK (record_version = 1),
      wake_json TEXT NOT NULL,
      peer_json TEXT NOT NULL,
      acknowledged INTEGER NOT NULL DEFAULT 0 CHECK (acknowledged IN (0, 1)),
      handed_off INTEGER NOT NULL DEFAULT 0 CHECK (handed_off IN (0, 1)),
      queued INTEGER NOT NULL DEFAULT 0 CHECK (queued IN (0, 1)),
      PRIMARY KEY (mailbox_id, target_key, through_sequence)
    );
";

fn initialize_mailbox_relay_schema(connection: &rusqlite::Connection) -> Result<()> {
    connection.execute_batch(MAILBOX_RELAY_SCHEMA_SQL)?;
    connection.execute(
        "INSERT INTO mailbox_relay_store(singleton,record_version,store_id) VALUES(1,1,?1)",
        [Uuid::new_v4().to_string()],
    )?;
    Ok(())
}

fn mailbox_relay_store_id(connection: &rusqlite::Connection) -> Result<String> {
    let (version, id): (i64, String) = connection
        .query_row(
            "SELECT record_version,store_id FROM mailbox_relay_store WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("mailbox relay store identity is missing or unreadable")?;
    if version != 1 || !mailbox_relay_uuid(&id) {
        bail!("unsupported or malformed mailbox relay store identity");
    }
    Ok(id)
}

fn validate_mailbox_relay_records(connection: &rusqlite::Connection) -> Result<()> {
    mailbox_relay_store_id(connection)?;
    let mut associations = connection.prepare(
        "SELECT record_version,pair_key,mailbox_id,conversation_json,names_json,acknowledged_through FROM mailbox_relay_associations",
    )?;
    let records = associations.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, u64>(5)?,
        ))
    })?;
    for record in records {
        let (version, pair, mailbox, json, names, _acknowledged_through) = record?;
        let conversation: MailboxRelayConversation = serde_json::from_str(&json)?;
        conversation
            .validate()
            .map_err(|error| anyhow!(error.message))?;
        let names: [String; 2] = serde_json::from_str(&names)?;
        if version != 1
            || pair != conversation.pair_key()
            || !valid_mailbox_relay_id(&mailbox)
            || names.iter().any(|name| !valid_mailbox_relay_name(name))
        {
            bail!("unsupported or malformed mailbox relay association");
        }
    }
    let mut intents = connection.prepare(
        "SELECT record_version,sender_key,idempotency_key,request_digest,conversation_json,peer_json,receipt_json,owner_session_id,requested_target FROM mailbox_relay_intents",
    )?;
    let records = intents.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
        ))
    })?;
    for record in records {
        let (version, sender, key, digest, json, peer, receipt, owner, requested_target) = record?;
        let conversation: MailboxRelayConversation = serde_json::from_str(&json)?;
        conversation
            .validate()
            .map_err(|error| anyhow!(error.message))?;
        let peer: FederationPeerSnapshot = serde_json::from_str(&peer)?;
        peer.validate().map_err(|error| anyhow!(error.message))?;
        let source_store = mailbox_relay_store_id(connection)?;
        if version != 1
            || requested_target.trim().is_empty()
            || requested_target.len() > MAX_MAILBOX_METADATA_BYTES
            || peer.central != conversation.home.host
            || key.trim().is_empty()
            || key.len() > MAX_MAILBOX_METADATA_BYTES
            || !conversation.participants.iter().any(|actor| {
                actor.participant_key() == sender
                    && actor.native_session_id == owner
                    && actor.host == peer.local
                    && actor.store_id == source_store
            })
            || !conversation
                .participants
                .iter()
                .any(|actor| actor.host == peer.peer)
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("unsupported or malformed mailbox relay intent");
        }
        if let Some(receipt) = receipt {
            let receipt: MailboxAppendReceipt = serde_json::from_str(&receipt)?;
            if !valid_mailbox_relay_id(&receipt.mailbox_id) || receipt.sequence == 0 {
                bail!("malformed mailbox relay append outcome");
            }
        }
    }
    let mut wakes = connection.prepare(
        "SELECT record_version,message_id,mailbox_id,principal_key,through_sequence,wake_json,peer_json FROM mailbox_relay_wakes",
    )?;
    let records = wakes.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, u64>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    for record in records {
        let (version, message, mailbox, principal, sequence, json, peer) = record?;
        let wake: MailboxRelayWake = serde_json::from_str(&json)?;
        wake.validate().map_err(|error| anyhow!(error.message))?;
        validate_mailbox_relay_wake_peer(&wake, &serde_json::from_str(&peer)?, true)?;
        if version != 1
            || wake.message_id != message
            || wake.mailbox_id != mailbox
            || wake.target.participant_key() != principal
            || wake.sequence != sequence
        {
            bail!("unsupported or malformed mailbox relay wake obligation");
        }
    }
    let mut notifications = connection.prepare(
        "SELECT record_version,mailbox_id,target_key,through_sequence,wake_json,peer_json FROM mailbox_relay_notifications",
    )?;
    let records = notifications.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, u64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    for record in records {
        let (version, mailbox, target, sequence, json, peer) = record?;
        let wake: MailboxRelayWake = serde_json::from_str(&json)?;
        wake.validate().map_err(|error| anyhow!(error.message))?;
        validate_mailbox_relay_wake_peer(&wake, &serde_json::from_str(&peer)?, false)?;
        if version != 1
            || wake.mailbox_id != mailbox
            || wake.target.participant_key() != target
            || wake.sequence != sequence
        {
            bail!("unsupported or malformed native relay notification");
        }
    }
    Ok(())
}
