// Exercise disposable databases through the production admission owners.
use super::*;

fn primary(activated: bool) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    ensure_sqlite_state_schema(&connection).unwrap();
    if activated {
        let journal = EngramImportReconciliationJournal::activated_empty();
        for ((field, key), (kind, encoded)) in engram_import_reconciliation_units(&journal).unwrap()
        {
            connection
                .execute(
                    "INSERT INTO state_metadata_units VALUES(?1,?2,?3,?4)",
                    rusqlite::params![field, key, kind, encoded],
                )
                .unwrap();
        }
    }
    let version = if activated {
        SQLITE_TEST_REQUEST_CUSTODY_IMPORT_PREDECESSOR
    } else {
        SQLITE_TEST_REQUEST_CUSTODY_PREDECESSOR
    };
    connection
        .execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            [version],
        )
        .unwrap();
    connection
}

fn primary_image(connection: &rusqlite::Connection) -> (String, u64, PersistMetadataUnits) {
    (
        required_state_meta_value(connection, "schema_version").unwrap(),
        metadata_unit_generation(connection).unwrap(),
        read_metadata_units_unchecked(connection).unwrap(),
    )
}

#[test]
fn ordinary_and_activated_primary_custody_migrations_preserve_raw_metadata() {
    for activated in [false, true] {
        let connection = primary(activated);
        let before = primary_image(&connection);
        prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap();
        let after = primary_image(&connection);
        assert_eq!(
            after.0,
            if activated {
                SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
            } else {
                SQLITE_SCHEMA_VERSION
            }
        );
        assert_eq!(after.1, before.1 + 1);
        assert_eq!(
            after.2, before.2,
            "migration must not normalize or drop predecessor custody"
        );
        assert!(
            validate_sqlite_state_schema_for_version(&connection, &before.0).is_err(),
            "a predecessor reader must refuse before loading the upgraded metadata"
        );
        prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap();
        assert_eq!(primary_image(&connection), after, "reopening is idempotent");
        assert_eq!(
            engram_import_reconciliation_floor_is_successor(&connection).unwrap(),
            activated
        );
    }
}

#[test]
fn primary_custody_faults_rollback_header_generation_and_metadata_together() {
    for activated in [false, true] {
        for phase in ["main_validated", "main_header", "main_readback"] {
            let connection = primary(activated);
            let before = primary_image(&connection);
            TEST_REQUEST_CUSTODY_FLOOR_FAULT.with(|slot| slot.set(Some(phase)));
            let result = prepare_existing_sqlite_state_reader_floor(&connection, true);
            TEST_REQUEST_CUSTODY_FLOOR_FAULT.with(|slot| slot.set(None));
            assert!(result.is_err(), "missing injected failure at {phase}");
            assert_eq!(
                primary_image(&connection),
                before,
                "partial migration escaped at {phase}"
            );
            prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap();
        }
    }
}

#[test]
fn predecessor_refuses_nested_custody_presence_before_typed_loading() {
    for activated in [false, true] {
        for field in [
            "testRunRegistrations",
            "testRunCompacted",
            "testRunWaits",
            "remoteTestRunFacts",
        ] {
            for key in ["causalRequest", "causal_request"] {
                let connection = primary(activated);
                let encoded = json!({"nested": [{"origin": {(key): null}}]}).to_string();
                connection
                    .execute(
                        "INSERT INTO state_metadata_units VALUES(?1,'/unversioned','entry',?2)",
                        rusqlite::params![field, encoded],
                    )
                    .unwrap();
                let before = primary_image(&connection);
                let count = PERSISTED_STATE_METADATA_DESERIALIZE_COUNT.with(|count| count.get());
                let error = prepare_existing_sqlite_state_reader_floor(&connection, true)
                    .err()
                    .expect("nested custody must be refused before typed loading");
                assert!(
                    error.to_string().contains("test-request custody"),
                    "{error:#}"
                );
                assert_eq!(
                    PERSISTED_STATE_METADATA_DESERIALIZE_COUNT.with(|count| count.get()),
                    count
                );
                assert_eq!(primary_image(&connection), before);
            }
        }
    }
}

fn coordination() -> rusqlite::Connection {
    coordination_at_floor(COORDINATION_TEST_REQUEST_CUSTODY_PREDECESSOR)
}

// Build old stores from their frozen definitions, never by opening current
// and relabeling it. These two boundaries exercise the full ordered upgrade
// and the immediately preceding production floor independently.
fn coordination_at_floor(version: &str) -> rusqlite::Connection {
    assert!(matches!(
        version,
        "1" | COORDINATION_TEST_REQUEST_CUSTODY_PREDECESSOR
    ));
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(COORDINATION_SCHEMA_V1_SQL)
        .unwrap();
    if version != "1" {
        connection
            .execute_batch(MAILBOX_READ_PAGES_SCHEMA_SQL)
            .unwrap();
        connection
            .execute_batch(FEDERATION_MAILBOX_DELIVERY_SCHEMA_SQL)
            .unwrap();
        connection
            .execute_batch(FEDERATION_EXECUTOR_SCHEMA_SQL)
            .unwrap();
        connection
            .execute_batch(MAILBOX_CONTINUATION_DECLARATIONS_SCHEMA_SQL)
            .unwrap();
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
    }
    connection
        .execute(
            "INSERT INTO meta VALUES('coordination_schema_version',?1)",
            [version],
        )
        .unwrap();
    connection
        .execute_batch(
            "INSERT INTO mailboxes VALUES('box','pair','stamp',2);
        INSERT INTO mailbox_participants VALUES('box','sender','Sender',1,'stamp',NULL);
        INSERT INTO mailbox_participants VALUES('box','recipient','Recipient',0,'stamp',NULL);
        INSERT INTO mailbox_messages(id,mailbox_id,sequence,sender_session_id,sender_name,
          target_session_id,target_name,created_at,class,body,idempotency_key,
          unread_depth_at_append,notification_disposition)
        VALUES('original','box',1,'sender','Sender','recipient','Recipient','stamp','routine',
          'retained body','original-key',1,'durableButNotWoken');",
        )
        .unwrap();
    validate_coordination_schema_version(&connection, version).unwrap();
    connection
}

#[test]
fn frozen_oldest_and_immediate_coordination_stores_upgrade_without_losing_rows() {
    for version in ["1", COORDINATION_TEST_REQUEST_CUSTODY_PREDECESSOR] {
        let connection = coordination_at_floor(version);
        let before = retained_message(&connection);
        let old_identity = (version != "1").then(|| mailbox_relay_store_id(&connection).unwrap());
        let old_columns = connection
            .prepare("SELECT * FROM mailbox_messages")
            .unwrap()
            .column_count();
        assert_eq!(old_columns, 16, "fixture has the frozen pre-custody shape");
        let rows = |projection: &str| {
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {projection} FROM mailbox_messages ORDER BY id"
                ))
                .unwrap();
            statement
                .query_map([], |row| {
                    (0..16)
                        .map(|column| row.get::<_, rusqlite::types::Value>(column))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        let old_rows = rows("*");
        ensure_sqlite_coordination_schema(&connection).unwrap();
        validate_current_coordination_schema(&connection).unwrap();
        assert_eq!(
            rows(
                "id,mailbox_id,sequence,sender_session_id,sender_name,target_session_id,target_name,created_at,class,topic,state_stamp,body,idempotency_key,unread_depth_at_append,notification_disposition,dispatch_outcome"
            ),
            old_rows
        );
        assert_eq!(retained_message(&connection), before);
        assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages WHERE test_request_json IS NOT NULL OR test_request_scope_json IS NOT NULL", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
        if let Some(identity) = old_identity {
            assert_eq!(mailbox_relay_store_id(&connection).unwrap(), identity);
        }
        assert!(
            validate_coordination_schema_version(&connection, version).is_err(),
            "the old exact reader must refuse the custody extension"
        );
        connection.execute_batch("PRAGMA query_only=ON").unwrap();
        ensure_sqlite_coordination_schema(&connection).unwrap();
        assert_eq!(retained_message(&connection), before);
    }
}

fn retained_message(connection: &rusqlite::Connection) -> (String, String, u64, u64) {
    connection
        .query_row(
            "SELECT body,idempotency_key,sequence,
        (SELECT processed_through FROM mailbox_participants WHERE session_id='recipient')
        FROM mailbox_messages WHERE id='original'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

#[test]
fn coordination_custody_migration_preserves_messages_cursors_and_store_identity() {
    let connection = coordination();
    let message = retained_message(&connection);
    let identity = mailbox_relay_store_id(&connection).unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    assert_eq!(retained_message(&connection), message);
    assert_eq!(mailbox_relay_store_id(&connection).unwrap(), identity);
    let custody: Option<String> = connection
        .query_row(
            "SELECT test_request_json FROM mailbox_messages WHERE id='original'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        custody.is_none(),
        "ordinary old mail must not acquire custody"
    );
    assert!(
        validate_coordination_schema_version(
            &connection,
            COORDINATION_TEST_REQUEST_CUSTODY_PREDECESSOR
        )
        .is_err()
    );
    ensure_sqlite_coordination_schema(&connection).unwrap();
    assert_eq!(retained_message(&connection), message);
}

#[test]
fn coordination_custody_faults_rollback_schema_marker_and_records() {
    for phase in [
        "coord_validated",
        "coord_columns",
        "coord_header",
        "coord_readback",
    ] {
        let connection = coordination();
        let before = coordination_schema_objects(&connection).unwrap();
        let message = retained_message(&connection);
        TEST_REQUEST_CUSTODY_FLOOR_FAULT.with(|slot| slot.set(Some(phase)));
        let result = ensure_sqlite_coordination_schema(&connection);
        TEST_REQUEST_CUSTODY_FLOOR_FAULT.with(|slot| slot.set(None));
        assert!(result.is_err(), "missing injected failure at {phase}");
        assert_eq!(coordination_schema_objects(&connection).unwrap(), before);
        validate_coordination_schema_version(
            &connection,
            COORDINATION_TEST_REQUEST_CUSTODY_PREDECESSOR,
        )
        .unwrap();
        assert_eq!(retained_message(&connection), message);
        ensure_sqlite_coordination_schema(&connection).unwrap();
    }
}

#[test]
fn current_coordination_refuses_malformed_custody_without_dropping_it() {
    let connection = coordination();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    connection
        .execute(
            "UPDATE mailbox_messages SET test_request_json=?1 WHERE id='original'",
            ["{\"schemaVersion\":1,\"unknown\":true}"],
        )
        .unwrap();
    assert!(ensure_sqlite_coordination_schema(&connection).is_err());
    let retained: String = connection
        .query_row(
            "SELECT test_request_json FROM mailbox_messages WHERE id='original'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, "{\"schemaVersion\":1,\"unknown\":true}");
}
