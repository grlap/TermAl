//! Normalized metadata storage contracts on disposable SQLite stores.
//! The legacy DDL is frozen independently of current production constants.
//! No live store, provider runtime, sleep or elapsed-time assertion is used.

use super::*;

#[path = "persist_metadata_units_repair.rs"]
mod repair;

fn metadata_units_connection(fixture: &FenceFixture) -> &rusqlite::Connection {
    fixture.cache.connection.as_ref().unwrap()
}

fn metadata_units_seed_legacy(connection: &rusqlite::Connection, metadata: &PersistedState) {
    connection
        .execute_batch(include_str!("fixtures/state_schema2.sql"))
        .unwrap();
    connection
        .execute(
            "INSERT INTO meta(key,value) VALUES('schema_version','2')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meta(key,value) VALUES('prompt_history_storage_version','1')",
            [],
        )
        .unwrap();
    seed_federation_host_identity(connection).unwrap();
    connection
        .execute(
            "INSERT INTO app_state(key,value_json) VALUES(?1,?2)",
            rusqlite::params![
                SQLITE_METADATA_KEY,
                serde_json::to_string(metadata).unwrap()
            ],
        )
        .unwrap();
}

fn metadata_units_metadata(connection: &rusqlite::Connection) -> PersistedState {
    serde_json::from_str(&read_persisted_metadata_json(connection).unwrap().unwrap()).unwrap()
}

fn metadata_units_version(connection: &rusqlite::Connection) -> String {
    required_state_meta_value(connection, "schema_version").unwrap()
}

fn metadata_units_audit(connection: &rusqlite::Connection) {
    connection.execute_batch("
        CREATE TEMP TABLE metadata_unit_audit(field_name TEXT, item_key TEXT, operation TEXT, bytes INTEGER);
        CREATE TEMP TRIGGER metadata_unit_insert AFTER INSERT ON state_metadata_units BEGIN
          INSERT INTO metadata_unit_audit VALUES(new.field_name,new.item_key,'insert',length(CAST(new.value_json AS BLOB)));
        END;
        CREATE TEMP TRIGGER metadata_unit_update AFTER UPDATE ON state_metadata_units BEGIN
          INSERT INTO metadata_unit_audit VALUES(new.field_name,new.item_key,'update',length(CAST(new.value_json AS BLOB)));
        END;
        CREATE TEMP TRIGGER metadata_unit_delete AFTER DELETE ON state_metadata_units BEGIN
          INSERT INTO metadata_unit_audit VALUES(old.field_name,old.item_key,'delete',0);
        END;
        CREATE TEMP TRIGGER forbid_app_state_insert BEFORE INSERT ON app_state BEGIN
          SELECT RAISE(ABORT,'global-app-state-write');
        END;
        CREATE TEMP TRIGGER forbid_app_state_update BEFORE UPDATE ON app_state BEGIN
          SELECT RAISE(ABORT,'global-app-state-write');
        END;
        CREATE TEMP TRIGGER forbid_app_state_delete BEFORE DELETE ON app_state BEGIN
          SELECT RAISE(ABORT,'global-app-state-write');
        END;
    ").unwrap();
}

fn metadata_units_audit_count(connection: &rusqlite::Connection) -> u64 {
    connection
        .query_row("SELECT count(*) FROM metadata_unit_audit", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn metadata_units_session(fixture: &FenceFixture) -> String {
    fixture
        .inner
        .lock()
        .unwrap()
        .create_session(
            Agent::Codex,
            Some("original".into()),
            fixture.root.path().to_string_lossy().into_owned(),
            None,
            None,
        )
        .session
        .id
}

fn metadata_units_message(inner: &mut StateInner, session_id: &str, text: &str) -> String {
    let id = inner.next_message_id();
    let index = inner.find_session_index(session_id).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    record.session.messages.push(Message::Text {
        id: id.clone(),
        timestamp: stamp_now(),
        author: Author::You,
        text: text.into(),
        attachments: Vec::new(),
        expanded_text: None,
        source: None,
    });
    record.session.message_count = record.session.messages.len() as u32;
    record.session.messages_loaded = true;
    record.message_positions = build_message_positions(&record.session.messages);
    id
}

#[test]
fn metadata_units_fresh_store_reloads_exact_counters_and_session_content() {
    let mut fixture = FenceFixture::new();
    let id = metadata_units_session(&fixture);
    let (message_id, next_session, next_message) = {
        let mut inner = fixture.inner.lock().unwrap();
        let message_id = metadata_units_message(&mut inner, &id, "first\nżółw 🐢");
        inner.revision = 41;
        (
            message_id,
            inner.next_session_number,
            inner.next_message_number,
        )
    };
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    assert_eq!(metadata_units_version(connection), SQLITE_SCHEMA_VERSION);
    let legacy_rows: u64 = connection
        .query_row("SELECT count(*) FROM app_state", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        legacy_rows, 0,
        "fresh writes use only normalized metadata authority"
    );
    let loaded = load_state(&fixture.root.path().join("termal.sqlite"))
        .unwrap()
        .unwrap();
    assert_eq!(loaded.revision, 41);
    assert_eq!(loaded.next_session_number, next_session);
    assert_eq!(loaded.next_message_number, next_message);
    let record = &loaded.sessions[loaded.find_session_index(&id).unwrap()];
    assert_eq!(record.session.name, "original");
    assert_eq!(record.session.messages[0].id(), message_id);
    match &record.session.messages[0] {
        Message::Text { text, .. } => assert_eq!(text, "first\nżółw 🐢"),
        _ => panic!("text record was not restored"),
    }
}

#[test]
fn metadata_units_primary2_migration_preserves_raw_values_and_legacy_image() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let mut inner = StateInner::new();
    inner.revision = 73;
    inner.next_session_number = 410;
    inner.next_message_number = u64::MAX - 5;
    inner
        .ignored_discovered_codex_thread_ids
        .insert("kept-thread".into());
    let metadata = PersistedState::metadata_from_inner(&inner);
    metadata_units_seed_legacy(&connection, &metadata);
    let before: String = connection
        .query_row(
            "SELECT value_json FROM app_state WHERE key=?1",
            [SQLITE_METADATA_KEY],
            |row| row.get(0),
        )
        .unwrap();
    let loaded = ensure_sqlite_state_schema_for_load(&connection)
        .unwrap()
        .unwrap();
    assert_eq!(metadata_units_version(&connection), SQLITE_SCHEMA_VERSION);
    assert_eq!(
        serde_json::to_value(&loaded).unwrap(),
        serde_json::to_value(&metadata).unwrap()
    );
    let retained: String = connection
        .query_row(
            "SELECT value_json FROM app_state WHERE key=?1",
            [SQLITE_METADATA_KEY],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        retained, before,
        "the legacy row is provenance, never a hot mirror"
    );
    assert_eq!(
        metadata_counter(
            &read_metadata_units(&connection).unwrap(),
            "nextMessageNumber"
        )
        .unwrap(),
        u64::MAX - 5
    );
    ensure_sqlite_state_schema_for_load(&connection).unwrap();
}

#[test]
fn metadata_units_migration_failure_before_marker_rolls_back_every_unit() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    metadata_units_seed_legacy(
        &connection,
        &PersistedState::metadata_from_inner(&StateInner::new()),
    );
    let image: String = connection
        .query_row("SELECT value_json FROM app_state", [], |row| row.get(0))
        .unwrap();
    connection
        .execute_batch(
            "CREATE TEMP TRIGGER reject_migration_marker BEFORE UPDATE OF value ON meta
        WHEN old.key='schema_version' BEGIN SELECT RAISE(ABORT,'migration-marker-fault'); END;",
        )
        .unwrap();
    let error = migrate_metadata_units(&connection).unwrap_err();
    assert!(format!("{error:#}").contains("migration-marker-fault"));
    assert_eq!(metadata_units_version(&connection), "2");
    assert!(
        !sqlite_state_user_table_names(&connection)
            .unwrap()
            .contains("state_metadata_units")
    );
    let after: String = connection
        .query_row("SELECT value_json FROM app_state", [], |row| row.get(0))
        .unwrap();
    assert_eq!(image, after);
    connection
        .execute_batch("DROP TRIGGER reject_migration_marker;")
        .unwrap();
    ensure_sqlite_state_schema_for_load(&connection).unwrap();
    assert_eq!(metadata_units_version(&connection), SQLITE_SCHEMA_VERSION);
}

#[test]
fn metadata_units_uncommitted_migration_ddl_disappears_after_connection_loss() {
    let root = TestTempRoot::create("metadata-migration-interruption");
    let path = root.path().join("termal.sqlite");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        metadata_units_seed_legacy(
            &connection,
            &PersistedState::metadata_from_inner(&StateInner::new()),
        );
        connection.execute_batch("BEGIN IMMEDIATE;").unwrap();
        connection
            .execute_batch(SQLITE_METADATA_UNITS_SCHEMA)
            .unwrap();
        connection
            .execute(
                "INSERT INTO state_metadata_units VALUES('partial','','value','1')",
                [],
            )
            .unwrap();
        // SQLite connection teardown rolls back an uncommitted migration,
        // exercising the same durable boundary as a pre-commit process loss.
    }
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(metadata_units_version(&connection), "2");
    assert!(
        !sqlite_state_user_table_names(&connection)
            .unwrap()
            .contains("state_metadata_units")
    );
    ensure_sqlite_state_schema_for_load(&connection).unwrap();
}

#[test]
fn metadata_units_revision_only_and_unchanged_ticks_never_write_app_state() {
    let mut fixture = FenceFixture::new();
    fixture.write(&fixture.collect()).unwrap();
    metadata_units_audit(metadata_units_connection(&fixture));
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(
        metadata_units_audit_count(metadata_units_connection(&fixture)),
        0
    );
    fixture.inner.lock().unwrap().revision += 1;
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    let fields: Vec<String> = connection
        .prepare("SELECT field_name FROM metadata_unit_audit")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(fields, vec!["revision"]);
}

#[test]
fn metadata_units_prompt_and_session_mutations_write_only_small_units() {
    let mut fixture = FenceFixture::new();
    let id = metadata_units_session(&fixture);
    fixture.write(&fixture.collect()).unwrap();
    metadata_units_audit(metadata_units_connection(&fixture));
    {
        let mut inner = fixture.inner.lock().unwrap();
        metadata_units_message(&mut inner, &id, "ordinary prompt");
        let index = inner.find_session_index(&id).unwrap();
        inner.session_mut_by_index(index).unwrap().session.name = "renamed".into();
        inner.revision += 1;
    }
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    let forbidden: u64 = connection
        .query_row(
            "SELECT count(*) FROM metadata_unit_audit
        WHERE field_name NOT IN ('revision','nextSessionNumber','nextMessageNumber')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(forbidden, 0);
    let bytes: u64 = connection
        .query_row(
            "SELECT coalesce(sum(bytes),0) FROM metadata_unit_audit",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(bytes < 64, "counter writes must stay small: {bytes}");
    assert_eq!(
        metadata_units_metadata(connection).next_message_number,
        fixture.inner.lock().unwrap().next_message_number
    );
}

#[test]
fn metadata_units_one_collection_entry_and_deletion_are_independent() {
    let mut fixture = FenceFixture::new();
    // Modeled collection, not a measured production field-size distribution.
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids = (0..3000)
        .map(|index| format!("thread-{index:05}"))
        .collect();
    fixture.write(&fixture.collect()).unwrap();
    metadata_units_audit(metadata_units_connection(&fixture));
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids
        .insert("thread-zzzzz".into());
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    let bytes: u64 = connection
        .query_row(
            "SELECT coalesce(sum(bytes),0) FROM metadata_unit_audit",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        bytes < 512,
        "one entry must not rewrite the modeled collection: {bytes}"
    );
    connection
        .execute("DELETE FROM metadata_unit_audit", [])
        .unwrap();
    fixture
        .inner
        .lock()
        .unwrap()
        .ignored_discovered_codex_thread_ids
        .remove("thread-zzzzz");
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    let deletes: u64 = connection
        .query_row(
            "SELECT count(*) FROM metadata_unit_audit
        WHERE field_name='ignoredDiscoveredCodexThreadIds' AND operation='delete'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(deletes, 1);
    assert_eq!(
        metadata_units_metadata(connection)
            .ignored_discovered_codex_thread_ids
            .len(),
        3000
    );
}

#[test]
fn metadata_units_order_duplicates_empty_and_object_keys_round_trip() {
    let original = json!({"ordered":[{"id":"same"},{"id":"same"},{"id":"third"}],
        "empty":[],"emptyMap":{},"map":{"":"empty-key","a/b":"slash","żółw":"🐢"},"nullable":null});
    let units = metadata_value_to_units(original.clone()).unwrap();
    assert_eq!(metadata_units_to_value(&units).unwrap(), original);
}

#[test]
fn metadata_units_durable_counter_floor_survives_deletion_and_stale_snapshot() {
    let mut fixture = FenceFixture::new();
    let id = metadata_units_session(&fixture);
    fixture.write(&fixture.collect()).unwrap();
    let stale = fixture.collect();
    {
        let mut inner = fixture.inner.lock().unwrap();
        inner.next_session_number = 900;
        inner.next_message_number = 8000;
        inner.revision = 700;
        let index = inner.find_session_index(&id).unwrap();
        inner.remove_session_at(index).unwrap();
    }
    fixture.write(&fixture.collect()).unwrap();
    assert!(format!("{:#}", fixture.write(&stale).unwrap_err()).contains("stale"));
    let connection =
        open_sqlite_state_connection_unconfigured(&fixture.root.path().join("termal.sqlite"))
            .unwrap();
    let metadata = metadata_units_metadata(&connection);
    assert_eq!(metadata.next_session_number, 900);
    assert_eq!(metadata.next_message_number, 8000);
    assert_eq!(metadata.revision, 700);
}

#[test]
fn metadata_units_materialized_identities_raise_older_counter_snapshot() {
    let mut fixture = FenceFixture::new();
    let id = metadata_units_session(&fixture);
    {
        let mut inner = fixture.inner.lock().unwrap();
        metadata_units_message(&mut inner, &id, "kept");
    }
    let mut delta = fixture.collect();
    delta.changed_sessions[0].session.id = "session-700".into();
    if let Message::Text { id, .. } = &mut delta.changed_sessions[0].session.messages[0] {
        *id = "message-6000".into();
    }
    fixture.write(&delta).unwrap();
    let metadata = metadata_units_metadata(metadata_units_connection(&fixture));
    assert_eq!(metadata.next_session_number, 701);
    assert_eq!(metadata.next_message_number, 6001);
}

#[test]
fn metadata_units_late_exact_fence_can_ack_without_any_metadata_dml() {
    let mut fixture = FenceFixture::new();
    let expected = fixture.wait_record("exact-late-wait");
    fixture.write(&fixture.collect()).unwrap();
    metadata_units_audit(metadata_units_connection(&fixture));
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    fixture.receive(false);
    assert_eq!(poll(&waiter), None);
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&waiter), Some(Ok(())));
    assert_eq!(
        metadata_units_audit_count(metadata_units_connection(&fixture)),
        0
    );
}

#[test]
fn metadata_units_mismatched_fence_and_expired_deadline_never_ack() {
    let mut fixture = FenceFixture::new();
    let mut expected = fixture.wait_record("kept-wait");
    fixture.write(&fixture.collect()).unwrap();
    expected.title = Some("different".into());
    let mismatch = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    fixture.receive(false);
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&mismatch), None);
    assert_eq!(
        mismatch.completion.poll_at(mismatch.completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    );
    fixture.batch.expire_resolved();
    assert!(!fixture.batch.has_pending());
}

#[test]
fn metadata_units_failed_transaction_does_not_advance_cache_or_ack() {
    let mut fixture = FenceFixture::new();
    fixture.write(&fixture.collect()).unwrap();
    let original = metadata_units_metadata(metadata_units_connection(&fixture)).next_message_number;
    let expected = fixture.wait_record("write-fault");
    fixture.inner.lock().unwrap().next_message_number += 1;
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected.clone()));
    fixture.receive(false);
    metadata_units_connection(&fixture)
        .execute_batch(
            "CREATE TEMP TRIGGER reject_counter_update
        BEFORE UPDATE ON state_metadata_units WHEN new.field_name='nextMessageNumber'
        BEGIN SELECT RAISE(ABORT,'counter-write-fault'); END;",
        )
        .unwrap();
    let delta = fixture.collect();
    let error = fixture.write(&delta).unwrap_err();
    assert!(format!("{error:#}").contains("counter-write-fault"));
    assert!(fixture.cache.connection.is_none());
    assert!(fixture.cache.metadata_units.is_none());
    assert!(matches!(
        poll(&waiter),
        Some(Err(PersistFenceError::WriteFailed(_)))
    ));
    {
        let connection =
            open_sqlite_state_connection_unconfigured(&fixture.root.path().join("termal.sqlite"))
                .unwrap();
        assert_eq!(
            metadata_units_metadata(&connection).next_message_number,
            original
        );
        assert!(
            !metadata_units_metadata(&connection)
                .delegation_waits
                .iter()
                .any(|wait| wait.id == expected.id)
        );
    }
    // The failed connection's TEMP trigger is gone. This is validation of a
    // named injected fault, not an unchanged retry of an unexplained failure.
    fixture.write(&delta).unwrap();
    assert_eq!(
        metadata_units_metadata(metadata_units_connection(&fixture)).next_message_number,
        original + 1
    );
    assert!(
        matches!(poll(&waiter), Some(Err(PersistFenceError::WriteFailed(_)))),
        "a failed receipt stays failed"
    );
    let late = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    fixture.receive(false);
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&late), Some(Ok(())));
}

#[test]
fn metadata_units_stale_writer_cannot_erase_a_newer_synchronous_global_change() {
    let mut fixture = FenceFixture::new();
    fixture.write(&fixture.collect()).unwrap();
    let stale = fixture.collect();
    let mut newer = stale.metadata.metadata_only();
    newer
        .ignored_discovered_codex_thread_ids
        .insert("synchronous-newer".into());
    newer.revision += 1;
    persist_state_from_persisted(&fixture.root.path().join("termal.sqlite"), &newer).unwrap();
    let result = fixture.write(&stale);
    if let Err(error) = result {
        assert!(
            format!("{error:#}").contains("stale") || format!("{error:#}").contains("concurrent")
        );
    }
    let connection =
        open_sqlite_state_connection_unconfigured(&fixture.root.path().join("termal.sqlite"))
            .unwrap();
    assert!(
        metadata_units_metadata(&connection)
            .ignored_discovered_codex_thread_ids
            .contains("synchronous-newer"),
        "a generation refresh must not turn an old desired image into deletion of a newer writer's unit"
    );
}

#[test]
fn metadata_units_missing_collection_descriptor_is_a_startup_failure() {
    let mut fixture = FenceFixture::new();
    fixture.wait_record("required-ledger");
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    connection
        .execute(
            "DELETE FROM state_metadata_units WHERE field_name='delegationWaits'",
            [],
        )
        .unwrap();
    assert!(
        ensure_sqlite_state_schema_for_load(connection).is_err(),
        "losing a safety ledger must not silently deserialize as its empty default"
    );
}

#[test]
fn metadata_units_missing_last_collection_entry_is_a_startup_failure() {
    let mut fixture = FenceFixture::new();
    fixture.wait_record("required-last-entry");
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    connection
        .execute(
            "DELETE FROM state_metadata_units
        WHERE field_name='delegationWaits' AND item_key<>''",
            [],
        )
        .unwrap();
    assert!(
        ensure_sqlite_state_schema_for_load(connection).is_err(),
        "an intact descriptor cannot make a missing trailing ledger entry look complete"
    );
}

#[test]
fn metadata_units_missing_required_counter_and_array_gap_are_rejected() {
    let mut fixture = FenceFixture::new();
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    connection
        .execute(
            "DELETE FROM state_metadata_units WHERE field_name='nextSessionNumber'",
            [],
        )
        .unwrap();
    assert!(ensure_sqlite_state_schema_for_load(connection).is_err());
    let mut units = metadata_value_to_units(json!({"ordered":[1,2]})).unwrap();
    units.remove(&("ordered".into(), "/00000000000000000000".into()));
    assert!(metadata_units_to_value(&units).is_err());
}

#[test]
fn metadata_units_u64_counter_values_are_exact_and_exhaustion_fails_closed() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    ensure_sqlite_state_schema(&connection).unwrap();
    let mut metadata = PersistedState::metadata_from_inner(&StateInner::new());
    metadata.next_message_number = u64::MAX - 1;
    write_metadata_units(
        &connection,
        project_metadata_units(&metadata).unwrap(),
        None,
        &[],
        None,
        false,
    )
    .unwrap();
    assert_eq!(
        metadata_units_metadata(&connection).next_message_number,
        u64::MAX - 1
    );
    let mut units = project_metadata_units(&metadata).unwrap();
    assert!(
        reserve_metadata_identity(
            &mut units,
            "nextMessageNumber",
            &format!("message-{}", u64::MAX),
            "message-"
        )
        .is_err()
    );
    let mut inner = StateInner::new();
    inner.next_message_number = u64::MAX;
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.next_message_id())).is_err()
    );
    assert_eq!(
        inner.next_message_number,
        u64::MAX,
        "no wrapped allocation is published"
    );
}

// Frozen primary-2 reader floor, from 0f1ec27d's schema validator. This is
// the old version guard, not a second invocation of the new format validator.
fn metadata_units_old_reader(connection: &rusqlite::Connection) -> Result<Value> {
    let version = required_state_meta_value(connection, "schema_version")?;
    if version != "2" {
        return Err(reject_unsupported_state_schema(format!(
            "found version `{version}`, expected `2`"
        )));
    }
    let encoded: String = connection.query_row(
        "SELECT value_json FROM app_state WHERE key=?1",
        [SQLITE_METADATA_KEY],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&encoded)?)
}

fn metadata_units_rollback_sql() -> String {
    include_str!("../../scripts/state-metadata-units-rollback.sql")
        .lines()
        .filter(|line| !line.starts_with('.'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn metadata_units_old_reader_refuses_before_using_stale_provenance() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    metadata_units_seed_legacy(
        &connection,
        &PersistedState::metadata_from_inner(&StateInner::new()),
    );
    assert!(metadata_units_old_reader(&connection).is_ok());
    ensure_sqlite_state_schema_for_load(&connection).unwrap();
    let before = connection.total_changes();
    let error = metadata_units_old_reader(&connection).unwrap_err();
    assert!(format!("{error:#}").contains("expected `2`"));
    assert_eq!(
        connection.total_changes(),
        before,
        "the refusal is before maintenance/write"
    );
}

#[test]
fn metadata_units_offline_rollback_materializes_current_counters_and_collections() {
    let mut fixture = FenceFixture::new();
    let id = metadata_units_session(&fixture);
    fixture.wait_record("rollback-kept");
    {
        let mut inner = fixture.inner.lock().unwrap();
        metadata_units_message(&mut inner, &id, "current message");
        inner.next_session_number = 901;
        inner.next_message_number = 8001;
        inner.revision = 7001;
    }
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    let expected = metadata_units_metadata(connection);
    connection
        .execute_batch(&metadata_units_rollback_sql())
        .unwrap();
    assert_eq!(metadata_units_version(connection), "2");
    assert!(
        !sqlite_state_user_table_names(connection)
            .unwrap()
            .contains("state_metadata_units")
    );
    let legacy: PersistedState =
        serde_json::from_value(metadata_units_old_reader(connection).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(legacy.next_session_number, 901);
    assert_eq!(legacy.next_message_number, 8001);
    let session_count: u64 = connection
        .query_row("SELECT count(*) FROM sessions WHERE id=?1", [&id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(session_count, 1);
    let message_count: u64 = connection
        .query_row(
            "SELECT count(*) FROM messages WHERE session_id=?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(message_count, 1);
    // The next new-reader boot can migrate that current primary-2 image again.
    let reloaded = ensure_sqlite_state_schema_for_load(connection)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(reloaded).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}
