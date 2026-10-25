// Exact historical upgrades and relay reader exclusions on disposable SQLite.
// Does not enable a peer or claim that protocol tests exercise delivery.
use super::*;

fn historical(connection: &rusqlite::Connection, version: &str) {
    connection
        .execute_batch(COORDINATION_SCHEMA_V1_SQL)
        .unwrap();
    if version != "1" {
        connection
            .execute_batch(MAILBOX_READ_PAGES_SCHEMA_SQL)
            .unwrap();
    }
    if matches!(version, "3" | "4" | "5") {
        connection
            .execute_batch(FEDERATION_MAILBOX_DELIVERY_SCHEMA_SQL)
            .unwrap();
    }
    if matches!(version, "4" | "5") {
        connection
            .execute_batch(FEDERATION_EXECUTOR_SCHEMA_SQL)
            .unwrap();
    }
    if version == "5" {
        connection
            .execute_batch(MAILBOX_CONTINUATION_DECLARATIONS_SCHEMA_SQL)
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO meta VALUES('coordination_schema_version',?1)",
            [version],
        )
        .unwrap();
    validate_coordination_schema_version(connection, version).unwrap();
}

fn version(connection: &rusqlite::Connection) -> String {
    connection
        .query_row(
            "SELECT value FROM meta WHERE key='coordination_schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn mailbox_relay_schema_historical_one_through_five_preserve_history_and_acknowledgements() {
    for predecessor in ["1", "2", "3", "4", "5"] {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        historical(&connection, predecessor);
        connection.execute_batch("
            INSERT INTO mailboxes VALUES('mailbox-old','old-pair','then',3);
            INSERT INTO mailbox_participants VALUES('mailbox-old','session-old','Old',1,'then',NULL);
            INSERT INTO mailbox_messages VALUES('old-message','mailbox-old',2,'session-peer','Peer',
              'session-old','Old','then','routine',NULL,NULL,'original unread body','original-key',1,
              'durableButNotWoken',NULL);
            INSERT INTO coordination_board_scopes VALUES('old-scope',1);
            INSERT INTO coordination_board_entries VALUES('old-scope','key',1,1,'42','session-old','Old','then',NULL);
        ").unwrap();
        if predecessor != "1" {
            connection.execute_batch("INSERT INTO mailbox_read_pages VALUES('old-receipt','mailbox-old','session-old',0,1);").unwrap();
        }
        ensure_sqlite_coordination_schema(&connection).unwrap();
        assert_eq!(version(&connection), "5-mailbox-relay-1");
        let identity = mailbox_relay_store_id(&connection).unwrap();
        assert!(mailbox_relay_uuid(&identity));
        assert_eq!(
            connection
                .query_row(
                    "SELECT body || ':' || idempotency_key FROM mailbox_messages",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "original unread body:original-key"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT processed_through FROM mailbox_participants",
                    [],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT value_json FROM coordination_board_entries",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "42"
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM mailbox_read_pages WHERE receipt='old-receipt'",
                    [],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            u64::from(predecessor != "1")
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM mailbox_relay_associations", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
        let schema = coordination_schema_objects(&connection).unwrap();
        let changes = connection.total_changes();
        assert!(
            validate_coordination_schema_version(&connection, "5").is_err(),
            "old exact reader must reject before DML"
        );
        assert_eq!(connection.total_changes(), changes);
        assert_eq!(coordination_schema_objects(&connection).unwrap(), schema);
        connection.execute_batch("PRAGMA query_only=ON").unwrap();
        ensure_sqlite_coordination_schema(&connection).unwrap();
        assert_eq!(mailbox_relay_store_id(&connection).unwrap(), identity);
    }
}

#[test]
fn mailbox_relay_schema_fresh_reopen_preserves_one_store_incarnation() {
    let root = TestTempRoot::create("mailbox-relay-store-incarnation");
    let path = root.path().join("coordination.sqlite");
    let first = bootstrap_coordination_database(&path).unwrap();
    let identity = mailbox_relay_store_id(&first).unwrap();
    drop(first);
    let reopened = bootstrap_coordination_database(&path).unwrap();
    assert_eq!(mailbox_relay_store_id(&reopened).unwrap(), identity);
    assert_eq!(version(&reopened), COORDINATION_SQLITE_SCHEMA_VERSION);
}

#[test]
fn mailbox_relay_schema_failed_upgrade_rolls_back_floor_and_all_new_tables() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    historical(&connection, "5");
    let before = coordination_schema_objects(&connection).unwrap();
    TEST_COORDINATION_UPGRADE_BEFORE_COMMIT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(|transaction| {
            assert_eq!(version(transaction), COORDINATION_SQLITE_SCHEMA_VERSION);
            mailbox_relay_store_id(transaction)?;
            bail!("injected relay pre-commit failure")
        }));
    });
    assert!(
        ensure_sqlite_coordination_schema(&connection)
            .unwrap_err()
            .to_string()
            .contains("pre-commit")
    );
    assert_eq!(version(&connection), "5");
    assert_eq!(coordination_schema_objects(&connection).unwrap(), before);
    validate_coordination_schema_version(&connection, "5").unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    validate_current_coordination_schema(&connection).unwrap();
}

#[test]
fn mailbox_relay_schema_concurrent_opener_observes_the_same_committed_identity() {
    let root = TestTempRoot::create("mailbox-relay-upgrade-race");
    let path = root.path().join("coordination.sqlite");
    let first = rusqlite::Connection::open(&path).unwrap();
    historical(&first, "5");
    let other_path = path.clone();
    let observed = Arc::new(Mutex::new(None));
    let witness = observed.clone();
    TEST_COORDINATION_BEFORE_UPGRADE.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let other = rusqlite::Connection::open(&other_path).unwrap();
            ensure_sqlite_coordination_schema(&other).unwrap();
            *witness.lock().unwrap() = Some(mailbox_relay_store_id(&other).unwrap());
        }));
    });
    ensure_sqlite_coordination_schema(&first).unwrap();
    assert_eq!(
        observed.lock().unwrap().as_ref(),
        Some(&mailbox_relay_store_id(&first).unwrap())
    );
    assert_eq!(version(&first), COORDINATION_SQLITE_SCHEMA_VERSION);
}

#[test]
fn mailbox_relay_schema_partial_unknown_or_malformed_target_is_not_repaired() {
    for mutation in [
        "DROP TABLE mailbox_relay_associations",
        "UPDATE meta SET value='5-mailbox-relay-2' WHERE key='coordination_schema_version'",
        "UPDATE mailbox_relay_store SET store_id='invalid'",
        "DELETE FROM mailbox_relay_store",
        "INSERT INTO mailbox_relay_associations(pair_key,record_version,mailbox_id,conversation_json,names_json) VALUES('pair',1,'mailbox-old','{}','[]')",
    ] {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        ensure_sqlite_coordination_schema(&connection).unwrap();
        connection.execute_batch(mutation).unwrap();
        let schema = coordination_schema_objects(&connection).unwrap();
        let marker = version(&connection);
        let changes = connection.total_changes();
        assert!(
            ensure_sqlite_coordination_schema(&connection).is_err(),
            "{mutation}"
        );
        assert_eq!(connection.total_changes(), changes, "{mutation}");
        assert_eq!(version(&connection), marker);
        assert_eq!(coordination_schema_objects(&connection).unwrap(), schema);
    }
}
