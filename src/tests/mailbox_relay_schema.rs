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
    if matches!(version, "3" | "4" | "5" | "5-mailbox-relay-1" | "6-executor-outcomes-1") {
        connection
            .execute_batch(FEDERATION_MAILBOX_DELIVERY_SCHEMA_SQL)
            .unwrap();
    }
    if matches!(version, "4" | "5" | "5-mailbox-relay-1" | "6-executor-outcomes-1") {
        connection
            .execute_batch(FEDERATION_EXECUTOR_SCHEMA_SQL)
            .unwrap();
    }
    if matches!(version, "5" | "5-mailbox-relay-1" | "6-executor-outcomes-1") {
        connection
            .execute_batch(MAILBOX_CONTINUATION_DECLARATIONS_SCHEMA_SQL)
            .unwrap();
    }
    if matches!(version, "5-mailbox-relay-1" | "6-executor-outcomes-1") {
        initialize_mailbox_relay_schema(connection).unwrap();
    }
    if version == "6-executor-outcomes-1" {
        connection.execute_batch(FEDERATION_EXECUTOR_OUTCOMES_SCHEMA_SQL).unwrap();
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
        assert_eq!(version(&connection), COORDINATION_SQLITE_SCHEMA_VERSION);
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

// Coordination step 6 adds central's executor outcome table. The relay floor
// upgrades in place: its store identity and records are kept, never minted
// again, and a newer unknown floor is refused rather than adopted.
#[test]
fn executor_outcome_floor_upgrades_the_relay_floor_and_refuses_unknown_futures() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch(COORDINATION_SCHEMA_V1_SQL).unwrap();
    connection.execute_batch(MAILBOX_READ_PAGES_SCHEMA_SQL).unwrap();
    connection.execute_batch(FEDERATION_MAILBOX_DELIVERY_SCHEMA_SQL).unwrap();
    connection.execute_batch(FEDERATION_EXECUTOR_SCHEMA_SQL).unwrap();
    connection.execute_batch(MAILBOX_CONTINUATION_DECLARATIONS_SCHEMA_SQL).unwrap();
    initialize_mailbox_relay_schema(&connection).unwrap();
    connection.execute("INSERT INTO meta VALUES('coordination_schema_version','5-mailbox-relay-1')", []).unwrap();
    validate_coordination_schema_version(&connection, "5-mailbox-relay-1").unwrap();
    let identity = mailbox_relay_store_id(&connection).unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    assert_eq!(version(&connection), COORDINATION_SQLITE_SCHEMA_VERSION);
    assert_eq!(COORDINATION_SQLITE_SCHEMA_VERSION, "7-review-custody-index-1");
    assert_eq!(mailbox_relay_store_id(&connection).unwrap(), identity, "the relay store identity is kept");
    assert_eq!(connection.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name='federation_executor_outcomes'",
        [], |row| row.get::<_, u64>(0)).unwrap(), 1);
    ensure_sqlite_coordination_schema(&connection).unwrap();
    assert_eq!(version(&connection), COORDINATION_SQLITE_SCHEMA_VERSION, "reopening the current floor is a no-op");

    for future in ["7", "6", "6-executor-outcomes-2", "7-direct-prompt-1"] {
        assert!(expected_coordination_schema_objects_for_version(future).is_err(), "{future}");
    }
    connection.execute("UPDATE meta SET value='7-direct-prompt-1' WHERE key='coordination_schema_version'", []).unwrap();
    let refused = ensure_sqlite_coordination_schema(&connection).unwrap_err();
    assert!(format!("{refused:#}").contains("unsupported coordination database schema"), "{refused:#}");
}

#[test]
fn delegation_review_custody_index_upgrades_step_six_and_bounds_orphan_lookup() {
    let root = TestTempRoot::create("review-custody-index-upgrade");
    let path = root.path().join("coordination.sqlite");
    let mut connection = rusqlite::Connection::open(&path).unwrap();
    historical(&connection, "6-executor-outcomes-1");
    let before = coordination_schema_objects(&connection).unwrap();
    let identity = mailbox_relay_store_id(&connection).unwrap();
    let body = "x".repeat(128 * 1024);
    let rows = 512;
    let transaction = connection.transaction().unwrap();
    transaction.execute("INSERT INTO mailboxes VALUES('fixture-mailbox','fixture-pair','then',513)", []).unwrap();
    for index in 0..rows {
        transaction.execute(
            "INSERT INTO mailbox_messages VALUES(?1,'fixture-mailbox',?2,'fixture-child','Child',
             'fixture-parent','Parent','then','routine',?3,NULL,?4,?5,1,'durableButNotWoken',NULL)",
            rusqlite::params![format!("message-{index}"), index + 1,
                if index < 8 { REVIEW_ATTEMPT_TOPIC } else { "unrelated-topic" }, body,
                if index < 8 { review_attempt_key("fixture-delegation", index + 1) } else { format!("unrelated-{index}") }],
        ).unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);
    let size = fs::metadata(&path).unwrap().len();
    // Measure a disposable 64 MiB message fixture, never the live store. This
    // includes existing schema admission and its one-time index construction.
    let started = std::time::Instant::now();
    let reopened = bootstrap_coordination_database(&path).unwrap();
    eprintln!("REVIEW_CUSTODY_INDEX_COST rows={rows} body_bytes={} store_bytes={size} boot_upgrade_us={}",
        body.len(), started.elapsed().as_micros());
    assert_eq!(mailbox_relay_store_id(&reopened).unwrap(), identity);
    assert_eq!(version(&reopened), COORDINATION_SQLITE_SCHEMA_VERSION);
    let after = coordination_schema_objects(&reopened).unwrap();
    let added = after.iter().filter(|object| !before.contains(object)).collect::<Vec<_>>();
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].name, "mailbox_messages_topic_key_sender");
    assert!(before.iter().all(|object| after.contains(object)), "step six objects stay frozen");
    assert_eq!(reopened.query_row("SELECT COUNT(*), SUM(length(body)) FROM mailbox_messages", [],
        |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?))).unwrap(), (512, 512 * body.len() as u64));
    let mut statement = reopened.prepare(&format!("EXPLAIN QUERY PLAN {REVIEW_CUSTODY_KEYS_WITHOUT_CHILD_SQL}")).unwrap();
    let plan = statement.query_map(rusqlite::params![Option::<String>::None,
        review_slot_prefix("fixture-delegation"), format!("{REVIEW_ATTEMPT_KEY_PREFIX}-fixture-delegation."),
        REVIEW_ATTEMPT_TOPIC], |row| row.get::<_, String>(3)).unwrap()
        .collect::<rusqlite::Result<Vec<_>>>().unwrap();
    assert!(plan.iter().any(|step| step.contains("SEARCH mailbox_messages USING COVERING INDEX mailbox_messages_topic_key_sender")
        && step.contains("idempotency_key>")), "{plan:?}");
    assert!(!plan.iter().any(|step| step.contains("SCAN ") || step.contains("TEMP B-TREE")), "{plan:?}");
    drop(statement);
    assert_eq!(review_slot_rows_on(&reopened, None, "fixture-delegation").unwrap().len(), 8);
    assert!(validate_coordination_schema_version(&reopened, "6-executor-outcomes-1").is_err(), "old reader rejects the new floor");
    drop(reopened);
    let reopened = bootstrap_coordination_database(&path).unwrap();
    assert_eq!(coordination_schema_objects(&reopened).unwrap(), after);
    assert_eq!(mailbox_relay_store_id(&reopened).unwrap(), identity);
}

#[test]
fn delegation_review_custody_index_upgrade_is_atomic_and_rejects_wrong_definitions() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    historical(&connection, "6-executor-outcomes-1");
    let before = coordination_schema_objects(&connection).unwrap();
    TEST_COORDINATION_UPGRADE_BEFORE_COMMIT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(|_| bail!("injected custody index pre-commit failure")));
    });
    assert!(ensure_sqlite_coordination_schema(&connection).unwrap_err().to_string().contains("pre-commit"));
    assert_eq!(coordination_schema_objects(&connection).unwrap(), before);
    assert_eq!(version(&connection), "6-executor-outcomes-1");
    connection.execute_batch("CREATE INDEX mailbox_messages_topic_key_sender ON mailbox_messages(topic)").unwrap();
    let changes = connection.total_changes();
    let wrong = coordination_schema_objects(&connection).unwrap();
    assert!(ensure_sqlite_coordination_schema(&connection).is_err());
    assert_eq!(connection.total_changes(), changes);
    assert_eq!(coordination_schema_objects(&connection).unwrap(), wrong);
    assert_eq!(version(&connection), "6-executor-outcomes-1");
}

#[test]
fn delegation_review_custody_index_concurrent_upgrade_revalidates_committed_floor() {
    let root = TestTempRoot::create("review-custody-index-concurrent-upgrade");
    let path = root.path().join("coordination.sqlite");
    let first = rusqlite::Connection::open(&path).unwrap();
    historical(&first, "6-executor-outcomes-1");
    let identity = mailbox_relay_store_id(&first).unwrap();
    let other_path = path.clone();
    TEST_COORDINATION_BEFORE_UPGRADE.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let other = rusqlite::Connection::open(&other_path).unwrap();
            ensure_sqlite_coordination_schema(&other).unwrap();
        }));
    });
    ensure_sqlite_coordination_schema(&first).unwrap();
    assert_eq!(version(&first), COORDINATION_SQLITE_SCHEMA_VERSION);
    assert_eq!(mailbox_relay_store_id(&first).unwrap(), identity);
    validate_current_coordination_schema(&first).unwrap();
}
