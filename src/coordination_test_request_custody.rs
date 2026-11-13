// Add columns through the existing coordination migration transaction. The
// historical table definitions remain frozen for schema admission and import.
const MAILBOX_TEST_REQUEST_CUSTODY_SCHEMA_SQL: &str = "
    ALTER TABLE mailbox_messages ADD COLUMN test_request_json TEXT;
    ALTER TABLE mailbox_messages ADD COLUMN test_request_scope_json TEXT;
    ALTER TABLE mailbox_relay_intents ADD COLUMN test_request_json TEXT;
    ALTER TABLE mailbox_relay_intents ADD COLUMN test_request_scope_json TEXT;
";

// Historical fixtures may start with a populated current store, but must
// remove this extension before claiming an older exact schema. Never let a
// downgrade helper discard custody to make that old-reader control pass.
#[cfg(test)]
fn remove_empty_test_request_custody_columns_for_test(connection: &rusqlite::Connection) {
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

fn validate_coordination_test_request_custody(connection: &rusqlite::Connection) -> Result<()> {
    for table in ["mailbox_messages", "mailbox_relay_intents"] {
        let mut statement = connection.prepare(&format!(
            "SELECT test_request_json,test_request_scope_json FROM {table}
             WHERE test_request_json IS NOT NULL OR test_request_scope_json IS NOT NULL"
        ))?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })?;
        for row in rows {
            let (encoded, scope) = row?;
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
