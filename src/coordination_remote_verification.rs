// Extends the ORIGINAL send owner through the existing boot-time transaction.
// Older readers refuse this floor before loading or rewriting any custody.
const MAILBOX_TEST_REQUEST_SETTLEMENT_SCHEMA_SQL: &str = "
    ALTER TABLE mailbox_messages ADD COLUMN test_request_settlement_json TEXT;
    ALTER TABLE mailbox_relay_intents ADD COLUMN test_request_settlement_json TEXT;
";

fn validate_coordination_test_request_settlement(connection: &rusqlite::Connection) -> Result<()> {
    for (table, canonical) in [
        ("mailbox_messages", "id"),
        ("mailbox_relay_intents", "receipt_json"),
    ] {
        let mut statement = connection.prepare(&format!(
            "SELECT test_request_json,test_request_scope_json,test_request_settlement_json,{canonical} FROM {table}
             WHERE test_request_settlement_json IS NOT NULL"))?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let payload = mailbox_test_payload_from_row(row, 0)?
                .ok_or_else(|| anyhow!("causal settlement without original test request"))?;
            let scope = mailbox_test_scope_from_row(row, 1)?;
            let id: String = row.get(3)?;
            let canonical_message_id = if table == "mailbox_relay_intents" {
                serde_json::from_str::<MailboxAppendReceipt>(&id)?.message_id
            } else {
                id
            };
            let selector = CausalTestCompletionSelector {
                canonical_message_id,
                payload,
            };
            let encoded: String = row.get(2)?;
            CausalTestSettlement::decode(&encoded, &selector, scope.as_ref())?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn remove_empty_test_request_settlement_columns_for_test(connection: &rusqlite::Connection) {
    for table in ["mailbox_messages", "mailbox_relay_intents"] {
        let columns = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        if !columns.iter().any(|c| c == "test_request_settlement_json") {
            continue;
        }
        let retained: u64 = connection
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {table} WHERE test_request_settlement_json IS NOT NULL"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            retained, 0,
            "historical fixture must not discard original settlement custody"
        );
        connection
            .execute_batch(&format!(
                "ALTER TABLE {table} DROP COLUMN test_request_settlement_json;"
            ))
            .unwrap();
    }
}
