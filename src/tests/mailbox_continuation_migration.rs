//! Exact landed executor-v4 integration into continuation-audit v5.
//! Disposable SQLite fixtures; no live store, runtime dispatch or remote proof.
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const RETAINED_TABLES: &[&str] = &[
    "mailboxes",
    "mailbox_participants",
    "mailbox_messages",
    "mailbox_read_pages",
    "federation_mailbox_intents",
    "federation_mailbox_dedup",
    "federation_mailbox_wake_obligations",
    "federation_mailbox_retired_namespaces",
    "federation_executor_operations",
];

fn retained_rows(connection: &rusqlite::Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    RETAINED_TABLES
        .iter()
        .filter(|table| {
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get::<_, u64>(0),
                )
                .unwrap()
                != 0
        })
        .map(|table| {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
                .unwrap();
            let columns = statement.column_count();
            statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|column| row.get(column))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        })
        .collect()
}

fn version(connection: &rusqlite::Connection) -> String {
    connection
        .query_row(
            "SELECT value FROM meta WHERE key='coordination_schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn populated_v4() -> (TestTempRoot, PathBuf) {
    let root = MailboxTestRoot::new();
    let path = root.database_path();
    let store = MailboxStore::open(&path).unwrap();
    let message = store.append(&test_input()).unwrap();
    let receipt = store
        .read_range("session-target", &message.mailbox_id, None, 10)
        .unwrap()
        .receipt
        .unwrap();
    store
        .acknowledge_page("session-target", &message.mailbox_id, &receipt)
        .unwrap();
    store
        .admit_federation_executor_operation(
            &FederationExecutorOperation {
                central_host_id: "migration-central".to_owned(),
                central_epoch: 7,
                operation_key: "pending-operation".to_owned(),
                target_host_id: "migration-executor".to_owned(),
                enrollment_generation: 3,
                session_id: "session-target".to_owned(),
                creation_id: "original-creation".to_owned(),
                placement_generation: 5,
                command_kind: "wake".to_owned(),
                payload: "retained executor payload".to_owned(),
            },
            || Ok(()),
        )
        .unwrap();
    {
        let connection = store.connection().unwrap();
        connection.execute_batch(
            "DROP TABLE mailbox_continuation_declarations;
             UPDATE meta SET value='4' WHERE key='coordination_schema_version';
             INSERT INTO federation_mailbox_intents
              (sender_session_id,idempotency_key,sender_host_id,owner_session_id,owner_creation_id,
               target_session_id,canonical_digest,envelope_json,state,payload,payload_bytes)
             VALUES('sender','key','host','owner','creation','target','digest','envelope',
                    'appendOutcomeUnknown','payload',7);
             INSERT INTO federation_mailbox_dedup
              (sender_host_id,sender_session_id,idempotency_key,target_session_id,canonical_digest,state)
             VALUES('host','sender','key','target','digest','refused');
             INSERT INTO federation_mailbox_retired_namespaces VALUES('retired','retired-at');"
        ).unwrap();
        connection.execute(
            "INSERT INTO federation_mailbox_wake_obligations VALUES(?1,'target','binding','pending')",
            [&message.message_id],
        ).unwrap();
        validate_coordination_schema_version(&connection, "4").unwrap();
    }
    drop(store);
    (root, path)
}

#[test]
fn mailbox_version_four_upgrade_preserves_exact_executor_and_mailbox_records() {
    let (_root, path) = populated_v4();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let before = retained_rows(&connection);
    assert!(before.iter().all(|rows| !rows.is_empty()));
    let executor = federation_executor_read(&connection, "migration-central", "pending-operation")
        .unwrap()
        .unwrap();
    assert_eq!(executor.stage, "received");
    ensure_sqlite_coordination_schema(&connection).unwrap();
    validate_current_coordination_schema(&connection).unwrap();
    assert_eq!(version(&connection), "5");
    assert_eq!(retained_rows(&connection), before);
    // Contract specimen of the old exact-v4 guard, not an old binary run.
    assert!(validate_coordination_schema_version(&connection, "4").is_err());
    assert_eq!(
        federation_executor_read(&connection, "migration-central", "pending-operation").unwrap(),
        Some(executor)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM mailbox_continuation_declarations",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    assert!(
        !connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    let schema = coordination_schema_objects(&connection).unwrap();
    let changes = connection
        .query_row("SELECT total_changes()", [], |row| row.get::<_, u64>(0))
        .unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT total_changes()", [], |row| row.get::<_, u64>(0))
            .unwrap(),
        changes
    );
    assert_eq!(coordination_schema_objects(&connection).unwrap(), schema);
    drop(connection);
    let reopened = MailboxStore::open(&path).unwrap();
    assert_eq!(retained_rows(&reopened.connection().unwrap()), before);
}

#[test]
fn mailbox_version_four_failed_upgrade_rolls_back_schema_marker_and_records() {
    let (_root, path) = populated_v4();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let schema = coordination_schema_objects(&connection).unwrap();
    let before = retained_rows(&connection);
    let reached = Arc::new(AtomicBool::new(false));
    let witness = reached.clone();
    TEST_COORDINATION_UPGRADE_BEFORE_COMMIT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |transaction| {
            witness.store(true, Ordering::SeqCst);
            validate_current_coordination_schema(transaction)?;
            assert_eq!(version(transaction), "5");
            bail!("injected coordination migration failure before COMMIT")
        }));
    });
    let error = ensure_sqlite_coordination_schema(&connection).unwrap_err();
    assert!(error.to_string().contains("before COMMIT"));
    assert!(reached.load(Ordering::SeqCst));
    assert_eq!(coordination_schema_objects(&connection).unwrap(), schema);
    assert_eq!(version(&connection), "4");
    assert_eq!(retained_rows(&connection), before);
    ensure_sqlite_coordination_schema(&connection).unwrap();
    assert_eq!(version(&connection), "5");
    assert_eq!(retained_rows(&connection), before);
}

#[test]
fn mailbox_version_four_concurrent_opener_rechecks_current_schema() {
    let (_root, path) = populated_v4();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let before = retained_rows(&connection);
    let upgraded = Arc::new(AtomicBool::new(false));
    let witness = upgraded.clone();
    let other_path = path.clone();
    TEST_COORDINATION_BEFORE_UPGRADE.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            // Deterministically interleave a real second connection between
            // source validation and the first opener's Immediate transaction.
            let other = rusqlite::Connection::open(&other_path).unwrap();
            ensure_sqlite_coordination_schema(&other).unwrap();
            assert_eq!(version(&other), "5");
            witness.store(true, Ordering::SeqCst);
        }));
    });
    ensure_sqlite_coordination_schema(&connection).unwrap();
    assert!(upgraded.load(Ordering::SeqCst));
    validate_current_coordination_schema(&connection).unwrap();
    assert_eq!(retained_rows(&connection), before);
    assert_eq!(
        connection
            .query_row("SELECT total_changes()", [], |row| row.get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn mailbox_version_four_unsupported_layouts_refuse_without_rewrite() {
    for change in [
        "audit-only",
        "partial",
        "modified",
        "extra",
        "mismatched",
        "missing",
        "unreadable",
    ] {
        let (_root, path) = populated_v4();
        let connection = rusqlite::Connection::open(&path).unwrap();
        match change {
            "audit-only" => {
                connection
                    .execute_batch("DROP TABLE federation_executor_operations;")
                    .unwrap();
                connection
                    .execute_batch(MAILBOX_CONTINUATION_DECLARATIONS_SCHEMA_SQL)
                    .unwrap();
            }
            "partial" => connection
                .execute_batch(MAILBOX_CONTINUATION_DECLARATIONS_SCHEMA_SQL)
                .unwrap(),
            "modified" => connection
                .execute_batch(
                    "ALTER TABLE federation_executor_operations ADD COLUMN surprise TEXT;",
                )
                .unwrap(),
            "extra" => connection
                .execute_batch("CREATE TABLE unexpected(value TEXT);")
                .unwrap(),
            "mismatched" => {
                connection
                    .execute(
                        "UPDATE meta SET value='3' WHERE key='coordination_schema_version'",
                        [],
                    )
                    .unwrap();
            }
            "missing" => {
                connection
                    .execute(
                        "DELETE FROM meta WHERE key='coordination_schema_version'",
                        [],
                    )
                    .unwrap();
            }
            "unreadable" => {
                connection
                    .execute(
                        "UPDATE meta SET value=x'00' WHERE key='coordination_schema_version'",
                        [],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let schema = coordination_schema_objects(&connection).unwrap();
        let records = retained_rows(&connection);
        let marker: Vec<(String, rusqlite::types::Value)> = connection
            .prepare("SELECT key,value FROM meta ORDER BY key")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        connection.execute_batch("PRAGMA query_only=ON").unwrap();
        let error = format!(
            "{:#}",
            ensure_sqlite_coordination_schema(&connection).unwrap_err()
        );
        if change == "unreadable" {
            assert!(
                error.contains("failed to read the coordination schema version"),
                "{error}"
            );
        } else {
            assert!(
                error.contains("unsupported coordination database schema"),
                "{change}: {error}"
            );
        }
        assert!(
            !error.contains("readonly"),
            "validation must precede writes: {error}"
        );
        assert_eq!(
            coordination_schema_objects(&connection).unwrap(),
            schema,
            "{change}"
        );
        assert_eq!(retained_rows(&connection), records, "{change}");
        let after: Vec<(String, rusqlite::types::Value)> = connection
            .prepare("SELECT key,value FROM meta ORDER BY key")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(after, marker, "{change}");
    }
}
