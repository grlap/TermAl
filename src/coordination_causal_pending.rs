// One-way coordination extension. Pending membership is written by the original
// send transaction; only this upgrade discovers pre-extension in-flight sends.

use super::*;
pub(crate) const MAILBOX_CAUSAL_PENDING_SCHEMA_SQL: &str = "
    ALTER TABLE mailbox_messages ADD COLUMN test_request_pending_json TEXT;
    ALTER TABLE mailbox_messages ADD COLUMN test_request_pending_order INTEGER;
    ALTER TABLE mailbox_relay_intents ADD COLUMN test_request_pending_json TEXT;
    ALTER TABLE mailbox_relay_intents ADD COLUMN test_request_pending_order INTEGER;
    CREATE INDEX mailbox_messages_causal_pending ON mailbox_messages(test_request_pending_order)
      WHERE test_request_pending_json IS NOT NULL;
    CREATE INDEX mailbox_relay_intents_causal_pending ON mailbox_relay_intents(test_request_pending_order)
      WHERE test_request_pending_json IS NOT NULL;
";

pub(super) fn seed_causal_pending(connection: &rusqlite::Connection) -> Result<()> {
    let store = mailbox_relay_store_id(connection)?;
    for intents in [false, true] {
        let table = CausalPendingRow { intents, row_id: 0 }.table();
        let owner = if intents {
            "owner_session_id"
        } else {
            "sender_session_id"
        };
        let canonical = if intents { "receipt_json" } else { "id" };
        let peer = if intents { "peer_json" } else { "NULL" };
        let mut statement = connection.prepare(&format!(
            "SELECT rowid,{owner},idempotency_key,test_request_json,test_request_scope_json,{canonical},{peer},test_request_settlement_json
             FROM {table} WHERE test_request_json IS NOT NULL OR test_request_scope_json IS NOT NULL OR test_request_settlement_json IS NOT NULL"))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let (reason, quarantine) =
                match causal_test_completion_plan_from_row(row, intents, &store) {
                    Ok(CausalPendingRead::Retired | CausalPendingRead::NotOriginal) => continue,
                    Ok(CausalPendingRead::Ready(_)) => {
                        (CausalPendingReason::AwaitingCompletion, false)
                    }
                    Ok(CausalPendingRead::Waiting(reason)) => (reason, false),
                    Err(reason) => (reason, true),
                };
            let mut state = CausalPendingState::new(reason, true);
            if quarantine {
                state.legacy_custody_digest = Some(causal_pending_custody_digest(row)?);
            }
            connection.execute(&format!(
                "UPDATE {table} SET test_request_pending_json=?1,test_request_pending_order=?2 WHERE rowid=?3"),
                rusqlite::params![serde_json::to_string(&state)?,next_causal_pending_order(connection)?,row.get::<_, i64>(0)?])?;
        }
    }
    Ok(())
}

// Hash exact SQLite values with type tags and lengths. Rowid is intentionally
// excluded: a VACUUM can move it without changing original custody. The digest
// stays private and authorizes only retention of the SAME known bad bytes.
fn causal_pending_custody_digest(row: &rusqlite::Row<'_>) -> Result<String> {
    use rusqlite::types::ValueRef;
    use sha2::Digest as _;
    let mut digest = sha2::Sha256::new();
    for column in 1..=7 {
        match row.get_ref(column)? {
            ValueRef::Null => digest.update([0]),
            ValueRef::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_le_bytes());
            }
            ValueRef::Real(value) => {
                digest.update([2]);
                digest.update(value.to_bits().to_le_bytes());
            }
            ValueRef::Text(value) | ValueRef::Blob(value) => {
                digest.update([if matches!(row.get_ref(column)?, ValueRef::Text(_)) {
                    3
                } else {
                    4
                }]);
                digest.update((value.len() as u64).to_le_bytes());
                digest.update(value);
            }
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) fn validate_coordination_causal_pending(
    connection: &rusqlite::Connection,
) -> Result<()> {
    for table in ["mailbox_messages", "mailbox_relay_intents"] {
        let mut statement = connection.prepare(&format!(
            "SELECT test_request_pending_json,test_request_pending_order FROM {table}
             WHERE test_request_pending_json IS NOT NULL OR test_request_pending_order IS NOT NULL"
        ))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let encoded: String = row.get(0)?;
            CausalPendingState::decode(&encoded)?;
            let order: i64 = row.get(1)?;
            if order < 1 {
                bail!("invalid causal pending retry order");
            }
        }
    }
    Ok(())
}

// A current pending marker is never permission to bypass boot custody checks.
// Only a failure explicitly retained by the one-time predecessor migration can
// be quarantined, and its original bytes must still fail the SAME closed check.
// Repaired rows and ordinary current sends therefore receive strict validation.
pub(super) fn causal_pending_legacy_quarantine(
    connection: &rusqlite::Connection,
    intents: bool,
) -> Result<BTreeSet<i64>> {
    let table = CausalPendingRow { intents, row_id: 0 }.table();
    let owner = if intents {
        "owner_session_id"
    } else {
        "sender_session_id"
    };
    let canonical = if intents { "receipt_json" } else { "id" };
    let peer = if intents { "peer_json" } else { "NULL" };
    let store = mailbox_relay_store_id(connection)?;
    let mut statement = connection.prepare(&format!(
        "SELECT rowid,{owner},idempotency_key,test_request_json,test_request_scope_json,{canonical},{peer},test_request_settlement_json,test_request_pending_json
         FROM {table} WHERE test_request_pending_json IS NOT NULL"))?;
    let mut rows = statement.query([])?;
    let mut quarantined = BTreeSet::new();
    while let Some(row) = rows.next()? {
        let state = CausalPendingState::decode(&row.get::<_, String>(8)?)?;
        if state.migrated
            && state.reason.custody_failure()
            && state.legacy_custody_digest.as_deref()
                == Some(causal_pending_custody_digest(row)?.as_str())
            && matches!(causal_test_completion_plan_from_row(row,intents,&store), Err(reason) if reason == state.reason)
        {
            quarantined.insert(row.get(0)?);
        }
    }
    Ok(quarantined)
}

#[cfg(test)]
pub(super) fn remove_empty_causal_pending_columns_for_test(connection: &rusqlite::Connection) {
    for table in ["mailbox_messages", "mailbox_relay_intents"] {
        let columns = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        if !columns.iter().any(|c| c == "test_request_pending_json") {
            continue;
        }
        let retained: u64 = connection.query_row(&format!(
            "SELECT COUNT(*) FROM {table} WHERE test_request_pending_json IS NOT NULL OR test_request_pending_order IS NOT NULL"),
            [], |row| row.get(0)).unwrap();
        assert_eq!(
            retained, 0,
            "historical fixture must not discard causal pending custody"
        );
        connection
            .execute_batch(&format!(
                "DROP INDEX {table}_causal_pending;
             ALTER TABLE {table} DROP COLUMN test_request_pending_order;
             ALTER TABLE {table} DROP COLUMN test_request_pending_json;"
            ))
            .unwrap();
    }
}
