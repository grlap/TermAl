// Metadata-only projections: SQL never selects payload, ACK text, queue IDs,
// attempt IDs or processed evidence. Presence is computed inside SQLite.
fn federation_journal_operation_rows(
    connection: &rusqlite::Connection,
    local_host: &str,
    high: i64,
    previous: Option<i64>,
    limit: usize,
) -> FederationJournalResult<Vec<(FederationJournalAnchor, Value)>> {
    connection
        .prepare(
            "SELECT rowid,central_host_id,operation_key,central_epoch,target_host_id,
         enrollment_generation,session_id,creation_id,placement_generation,command_kind,
         stage,effect_state,queue_entry_id IS NOT NULL,delivery_attempt_id IS NOT NULL,
         runtime_ack IS NOT NULL,processed_evidence_kind
         FROM federation_executor_operations WHERE rowid<=?1
         AND (?2 IS NULL OR rowid<?2) ORDER BY rowid DESC LIMIT ?3",
        )
        .map_err(|_| FederationJournalError::Storage)?
        .query_map(
            rusqlite::params![high, previous, (limit + 1) as u64],
            |row| {
                let central: String = row.get(1)?;
                let key: String = row.get(2)?;
                let target: String = row.get(4)?;
                let anchor = FederationJournalAnchor {
                    rowid: row.get(0)?,
                    identity: federation_journal_identity(&[&central, &key]),
                };
                let value = json!({
                    "operationKey":key, "commandKind":row.get::<_, String>(9)?,
                    "stage":row.get::<_, String>(10)?, "effectState":row.get::<_, String>(11)?,
                    "enrollmentGeneration":row.get::<_, u64>(5)?, "mirror":target != local_host,
                    "targetBinding":{
                        "central":{"hostId":central,"hostGeneration":row.get::<_, u64>(3)?},
                        "identity":{"hostId":target,"sessionId":row.get::<_, String>(6)?,
                            "creationId":row.get::<_, String>(7)?},
                        "placementGeneration":row.get::<_, u64>(8)?},
                    "queueEntryPresent":row.get::<_, bool>(12)?,
                    "deliveryAttemptPresent":row.get::<_, bool>(13)?,
                    "runtimeAckPresent":row.get::<_, bool>(14)?,
                    "processedEvidenceKind":row.get::<_, Option<String>>(15)?,
                });
                Ok((anchor, value))
            },
        )
        .map_err(|_| FederationJournalError::Storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| FederationJournalError::Storage)
}

struct FederationJournalWakeRow {
    anchor: FederationJournalAnchor,
    message: String,
    state: String,
    target: Option<FederationMailboxBinding>,
    mirror: bool,
    metadata_supported: Option<bool>,
}

fn federation_journal_wake_rows(
    connection: &rusqlite::Connection,
    central_host: &str,
    high: i64,
    previous: Option<i64>,
    limit: usize,
) -> FederationJournalResult<Vec<FederationJournalWakeRow>> {
    connection
        .prepare(
            "SELECT w.rowid,w.message_id,w.state,
         CASE WHEN length(CAST(w.target_binding_json AS BLOB))<=4096
              THEN w.target_binding_json ELSE NULL END,
         EXISTS(SELECT 1 FROM federation_executor_operations o
                WHERE o.central_host_id=?4 AND o.operation_key='mailbox-wake:'||w.message_id),
         m.mailbox_id,m.sequence,m.unread_depth_at_append,m.sender_session_id,
         CASE WHEN length(CAST(m.sender_name AS BLOB))<=16384 THEN m.sender_name ELSE NULL END,
         CASE WHEN length(CAST(m.topic AS BLOB))<=16384 THEN m.topic ELSE NULL END,
         m.id IS NOT NULL AND length(CAST(m.sender_name AS BLOB))<=16384
           AND (m.topic IS NULL OR length(CAST(m.topic AS BLOB))<=16384)
         FROM federation_mailbox_wake_obligations w
         LEFT JOIN mailbox_messages m ON m.id=w.message_id
         WHERE w.rowid<=?1 AND (?2 IS NULL OR w.rowid<?2)
         ORDER BY w.rowid DESC LIMIT ?3",
        )
        .map_err(|_| FederationJournalError::Storage)?
        .query_map(
            rusqlite::params![high, previous, (limit + 1) as u64, central_host],
            |row| {
                let message: String = row.get(1)?;
                let binding: Option<String> = row.get(3)?;
                let target: Option<FederationMailboxBinding> =
                    binding.and_then(|binding| serde_json::from_str(&binding).ok());
                // Validate bounded display metadata through the native total
                // normalizer. It is never returned; mailbox body is not selected.
                let metadata_supported = if row.get::<_, bool>(11)? {
                    match (
                        &target,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<u64>>(6)?,
                        row.get::<_, Option<u64>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                    ) {
                        (
                            Some(target),
                            Some(mailbox_id),
                            Some(sequence),
                            Some(unread_count),
                            Some(sender_session_id),
                            Some(sender_name),
                        ) => {
                            let wake = FederationExecutorWake {
                                mailbox_id,
                                message_id: message.clone(),
                                sequence,
                                unread_count,
                                sender_session_id,
                                sender_name,
                                topic: row.get(10)?,
                            }
                            .with_display_copies();
                            Some(
                                FederationExecutorWakeRequest {
                                    protocol_version: FEDERATION_PROTOCOL_VERSION,
                                    operation_key: format!("mailbox-wake:{message}"),
                                    target: target.clone(),
                                    wake,
                                }
                                .operation(1)
                                .is_ok(),
                            )
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                Ok(FederationJournalWakeRow {
                    anchor: FederationJournalAnchor {
                        rowid: row.get(0)?,
                        identity: federation_journal_identity(&[&message]),
                    },
                    message,
                    state: row.get(2)?,
                    target,
                    mirror: row.get(4)?,
                    metadata_supported,
                })
            },
        )
        .map_err(|_| FederationJournalError::Storage)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| FederationJournalError::Storage)
}
