// Add columns through the existing coordination migration transaction. The
// historical table definitions remain frozen for schema admission and import.

use super::*;
pub(crate) const MAILBOX_TEST_REQUEST_CUSTODY_SCHEMA_SQL: &str = "
    ALTER TABLE mailbox_messages ADD COLUMN test_request_json TEXT;
    ALTER TABLE mailbox_messages ADD COLUMN test_request_scope_json TEXT;
    ALTER TABLE mailbox_relay_intents ADD COLUMN test_request_json TEXT;
    ALTER TABLE mailbox_relay_intents ADD COLUMN test_request_scope_json TEXT;
";

// Historical fixtures may start with a populated current store, but must
// remove this extension before claiming an older exact schema. Never let a
// downgrade helper discard custody to make that old-reader control pass.
#[cfg(test)]
pub(crate) fn remove_empty_test_request_custody_columns_for_test(
    connection: &rusqlite::Connection,
) {
    remove_empty_test_request_settlement_columns_for_test(connection);
    for table in ["mailbox_messages", "mailbox_relay_intents"] {
        let retained: u64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE test_request_json IS NOT NULL OR test_request_scope_json IS NOT NULL"),
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(
            retained, 0,
            "historical fixture must not discard {table} custody"
        );
        connection
            .execute_batch(&format!(
                "ALTER TABLE {table} DROP COLUMN test_request_scope_json;
             ALTER TABLE {table} DROP COLUMN test_request_json;"
            ))
            .unwrap();
    }
}

pub(super) fn validate_coordination_test_request_custody(
    connection: &rusqlite::Connection,
) -> Result<()> {
    validate_coordination_test_request_custody_rows(connection, false)
}

pub(super) fn validate_coordination_test_request_custody_with_legacy_quarantine(
    connection: &rusqlite::Connection,
) -> Result<()> {
    validate_coordination_test_request_custody_rows(connection, true)
}

fn validate_coordination_test_request_custody_rows(
    connection: &rusqlite::Connection,
    allow_legacy_quarantine: bool,
) -> Result<()> {
    for table in ["mailbox_messages", "mailbox_relay_intents"] {
        let quarantined = if allow_legacy_quarantine {
            causal_pending_legacy_quarantine(connection, table == "mailbox_relay_intents")?
        } else {
            BTreeSet::new()
        };
        let mut statement = connection.prepare(&format!(
            "SELECT test_request_json,test_request_scope_json,rowid FROM {table}
             WHERE test_request_json IS NOT NULL OR test_request_scope_json IS NOT NULL"
        ))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            if quarantined.contains(&row.get::<_, i64>(2)?) {
                continue;
            }
            let encoded: Option<String> = row.get(0)?;
            let scope: Option<String> = row.get(1)?;
            let encoded =
                encoded.ok_or_else(|| anyhow!("sender scope has no retained test request"))?;
            if encoded.len() > MAILBOX_TEST_REQUEST_MAX_BYTES {
                bail!("persisted test-request custody exceeds its bound");
            }
            let payload: MailboxTestPayload = serde_json::from_str(&encoded)?;
            validate_mailbox_test_payload(&payload).map_err(|error| anyhow!(error.message))?;
            if let Some(encoded) = scope {
                if encoded.len() > MAILBOX_TEST_REQUEST_MAX_BYTES {
                    bail!("sender scope exceeds its bound");
                }
                let scope: MailboxTestSenderScope = serde_json::from_str(&encoded)?;
                if scope.project_id.is_empty()
                    || scope.store.project_id.is_empty()
                    || scope.store.database_path.as_os_str().is_empty()
                {
                    bail!("invalid sender test-request scope");
                }
            }
        }
    }
    Ok(())
}
