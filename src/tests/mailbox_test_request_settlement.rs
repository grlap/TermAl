// Deterministic exact predecessor, rollback and backup controls. No live store.
use super::*;

fn predecessor() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    for sql in [
        COORDINATION_SCHEMA_V1_SQL,
        MAILBOX_READ_PAGES_SCHEMA_SQL,
        FEDERATION_MAILBOX_DELIVERY_SCHEMA_SQL,
        FEDERATION_EXECUTOR_SCHEMA_SQL,
        MAILBOX_CONTINUATION_DECLARATIONS_SCHEMA_SQL,
    ] {
        connection.execute_batch(sql).unwrap();
    }
    initialize_mailbox_relay_schema(&connection).unwrap();
    connection
        .execute_batch(FEDERATION_EXECUTOR_OUTCOMES_SCHEMA_SQL)
        .unwrap();
    connection
        .execute_batch(MAILBOX_REVIEW_CUSTODY_INDEX_SCHEMA_SQL)
        .unwrap();
    connection
        .execute_batch(&remote_prompt_storage_schema_sql())
        .unwrap();
    connection
        .execute_batch(MAILBOX_TEST_REQUEST_CUSTODY_SCHEMA_SQL)
        .unwrap();
    connection
        .execute(
            "INSERT INTO meta VALUES('coordination_schema_version',?1)",
            [COORDINATION_REMOTE_VERIFICATION_PREDECESSOR],
        )
        .unwrap();
    validate_coordination_schema_version(&connection, COORDINATION_REMOTE_VERIFICATION_PREDECESSOR)
        .unwrap();
    connection
}

fn raw_rows(connection: &rusqlite::Connection) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
    let tables = connection.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap().query_map([], |r| r.get::<_,String>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    tables
        .into_iter()
        .map(|table| {
            let mut stmt = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
                .unwrap();
            let count = stmt.column_count();
            let rows = stmt
                .query_map([], |row| {
                    (0..count)
                        .map(|i| row.get(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (table, rows)
        })
        .collect()
}

#[test]
fn remote_settlement_floor_preserves_original_rows_and_refuses_the_old_reader() {
    let connection = predecessor();
    connection
        .execute_batch(
            "INSERT INTO mailboxes VALUES('old-box','pair','stamp',1);
        INSERT INTO mailbox_participants VALUES('old-box','sender','Sender',0,'stamp',NULL);
        INSERT INTO mailbox_messages(id,mailbox_id,sequence,sender_session_id,sender_name,
         target_session_id,target_name,created_at,class,body,idempotency_key,
         unread_depth_at_append,notification_disposition)
        VALUES('old-message','old-box',1,'sender','Sender','recipient','Recipient','stamp','routine','retained original body','old-key',1,'durableButNotWoken');",
        )
        .unwrap();
    let schema = coordination_schema_objects(&connection).unwrap();
    let identity = mailbox_relay_store_id(&connection).unwrap();
    let before = raw_rows(&connection);
    ensure_sqlite_coordination_schema(&connection).unwrap();
    let after = coordination_schema_objects(&connection).unwrap();
    for old in &schema {
        if !matches!(
            old.name.as_str(),
            "mailbox_messages" | "mailbox_relay_intents"
        ) {
            assert!(
                after.contains(old),
                "unaffected objects remain frozen: {}",
                old.name
            );
        }
    }
    for (table, old_rows) in before {
        if table == "meta" {
            continue;
        }
        let new_rows = raw_rows(&connection)
            .into_iter()
            .find(|(name, _)| name == &table)
            .unwrap()
            .1;
        let expected = old_rows
            .into_iter()
            .map(|mut row| {
                if matches!(table.as_str(), "mailbox_messages" | "mailbox_relay_intents") {
                    row.extend(std::iter::repeat_n(rusqlite::types::Value::Null, 3));
                }
                row
            })
            .collect::<Vec<_>>();
        assert_eq!(new_rows, expected, "{table} preserves every old value");
    }
    assert_eq!(mailbox_relay_store_id(&connection).unwrap(), identity);
    assert!(
        validate_coordination_schema_version(
            &connection,
            COORDINATION_REMOTE_VERIFICATION_PREDECESSOR
        )
        .is_err()
    );
    for table in ["mailbox_messages", "mailbox_relay_intents"] {
        let columns = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            &columns[columns.len() - 3..],
            &[
                ("test_request_settlement_json".into(), "TEXT".into(), 0),
                ("test_request_pending_json".into(), "TEXT".into(), 0),
                ("test_request_pending_order".into(), "INTEGER".into(), 0)
            ]
        );
    }
    connection.execute_batch("PRAGMA query_only=ON").unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
}

#[test]
fn remote_settlement_floor_failure_rolls_back_columns_marker_and_records() {
    for phase in ["coord_settlement_columns", "coord_header", "coord_readback"] {
        let connection = predecessor();
        let schema = coordination_schema_objects(&connection).unwrap();
        let rows = raw_rows(&connection);
        TEST_REQUEST_CUSTODY_FLOOR_FAULT.with(|slot| slot.set(Some(phase)));
        let result = ensure_sqlite_coordination_schema(&connection);
        TEST_REQUEST_CUSTODY_FLOOR_FAULT.with(|slot| slot.set(None));
        assert!(result.is_err(), "{phase}");
        assert_eq!(coordination_schema_objects(&connection).unwrap(), schema);
        assert_eq!(raw_rows(&connection), rows);
        validate_coordination_schema_version(
            &connection,
            COORDINATION_REMOTE_VERIFICATION_PREDECESSOR,
        )
        .unwrap();
    }
}

#[test]
fn malformed_or_orphan_settlement_is_refused_without_discarding_the_row() {
    let connection = predecessor();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    connection.execute_batch("INSERT INTO mailboxes VALUES('box','pair','stamp',2);
        INSERT INTO mailbox_participants VALUES('box','sender','Sender',0,'stamp',NULL);
        INSERT INTO mailbox_messages(id,mailbox_id,sequence,sender_session_id,sender_name,
         target_session_id,target_name,created_at,class,body,idempotency_key,
         unread_depth_at_append,notification_disposition,test_request_settlement_json)
        VALUES('original','box',1,'sender','Sender','recipient','Recipient','stamp','routine','body','key',1,'durableButNotWoken','{}');").unwrap();
    assert!(validate_current_coordination_schema(&connection).is_err());
    assert_eq!(
        connection
            .query_row(
                "SELECT test_request_settlement_json FROM mailbox_messages WHERE id='original'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "{}"
    );
}

#[path = "mailbox_test_request_settlement/pending.rs"]
mod pending;
