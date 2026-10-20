// Owns exact historical-schema upgrade and retained receipt controls.
// Does not relax schema inventories or reconstruct mailbox content from intents.
use super::*;

#[test]
fn federation_delivery_version_two_upgrade_preserves_messages_cursors_and_read_receipts() {
    let state = federation_mailboxes::central_store();
    let path = resolve_coordination_persistence_path(state.persistence_path.as_path());
    let input = MailboxAppendInput {
        sender_session_id: "old-sender".into(),
        sender_name: "Old sender".into(),
        target_session_id: "old-target".into(),
        target_name: "Old target".into(),
        body: "original content".into(),
        idempotency_key: "old-key".into(),
        topic: None,
        state_stamp: None,
    };
    let appended = state.mailbox_store.append(&input).unwrap();
    state
        .mailbox_store
        .record_initial_dispatch_outcome(&appended.receipt.message_id, "durableButNotWoken")
        .unwrap();
    drop(appended.finalization);
    let page = state
        .mailbox_store
        .read_range_authorized(
            "old-target",
            &appended.receipt.mailbox_id,
            None,
            10,
            true,
            || Ok(()),
        )
        .unwrap();
    let before = {
        let connection = state.mailbox_store.connection().unwrap();
        connection
            .execute_batch(
                "DROP TABLE mailbox_continuation_declarations;
             DROP TABLE federation_executor_operations;
             DROP TABLE federation_mailbox_wake_obligations;
             DROP TABLE federation_mailbox_dedup;
             DROP TABLE federation_mailbox_intents;
             DROP TABLE federation_mailbox_retired_namespaces;
             UPDATE meta SET value='2' WHERE key='coordination_schema_version';",
            )
            .unwrap();
        validate_coordination_schema_version(&connection, "2").unwrap();
        connection
            .query_row(
                "SELECT body,sequence FROM mailbox_messages WHERE id=?1",
                [&appended.receipt.message_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)),
            )
            .unwrap()
    };
    let upgraded = MailboxStore::open(&path).unwrap();
    upgraded
        .acknowledge_page(
            "old-target",
            &appended.receipt.mailbox_id,
            page.receipt.as_deref().unwrap(),
        )
        .unwrap();
    let connection = upgraded.connection().unwrap();
    validate_current_coordination_schema(&connection).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT body,sequence FROM mailbox_messages WHERE id=?1",
                [&appended.receipt.message_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            )
            .unwrap(),
        before
    );
    assert_eq!(
        mailbox_processed_through(&connection, &appended.receipt.mailbox_id, "old-target").unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM federation_mailbox_intents",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn federation_delivery_failed_version_two_upgrade_preserves_exact_historical_schema() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(COORDINATION_SCHEMA_V1_SQL)
        .unwrap();
    connection
        .execute_batch(MAILBOX_READ_PAGES_SCHEMA_SQL)
        .unwrap();
    connection
        .execute_batch(
            "INSERT INTO meta VALUES('coordination_schema_version','2'); PRAGMA query_only=ON;",
        )
        .unwrap();
    let before = coordination_schema_objects(&connection).unwrap();
    assert!(ensure_sqlite_coordination_schema(&connection).is_err());
    assert_eq!(coordination_schema_objects(&connection).unwrap(), before);
    validate_coordination_schema_version(&connection, "2").unwrap();
}
