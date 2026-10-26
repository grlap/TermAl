//! Concurrency, integrity upgrade and diagnostic controls.
use super::*;

#[test]
fn metadata_units_synchronous_source_captures_after_earlier_background_commit() {
    let mut fixture = FenceFixture::new();
    fixture.write(&fixture.collect()).unwrap();
    fixture.inner.lock().unwrap().ignored_discovered_codex_thread_ids.insert("background".into());
    let background_delta = fixture.collect();
    let path = fixture.root.path().join("termal.sqlite");
    let inner = fixture.inner.clone();
    let writer = sqlite_state_write_lock(&path);
    let barrier = lock_sqlite_state_writer(&writer);
    let issued = sqlite_state_writer_issued_tickets(&writer);
    let background = std::thread::spawn(move || {
        let result = fixture.write(&background_delta);
        (fixture, result)
    });
    // The cached writer is queued first and needs no StateInner lock to commit
    // and publish. The synchronous source must capture AFTER that publication.
    wait_for_sqlite_state_writer_issued_tickets(&writer, issued + 1);
    let sync_path = path.clone();
    let synchronous = std::thread::spawn(move || {
        let mut source = inner.lock().unwrap();
        source.ignored_discovered_codex_thread_ids.insert("synchronous".into());
        persist_state(&sync_path, &source)
    });
    // This ticket is synchronous schema setup. The old implementation already
    // captured the predecessor before queuing it; the source-owned path must not.
    wait_for_sqlite_state_writer_issued_tickets(&writer, issued + 2);
    drop(barrier);
    let (_fixture, background_result) = background.join().unwrap();
    background_result.unwrap();
    synchronous.join().unwrap().unwrap();
    let restored = load_state(&path).unwrap().unwrap();
    assert_eq!(restored.ignored_discovered_codex_thread_ids,
        BTreeSet::from(["background".to_owned(), "synchronous".to_owned()]));
}

#[test]
fn metadata_units_unknown_versions_refuse_all_metadata_readback_without_legacy_fallback() {
    let mut fixture = FenceFixture::new();
    fixture.inner.lock().unwrap().test_run_cards_epoch = Some("current-proof".into());
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    let legacy = read_persisted_metadata_json(connection).unwrap().unwrap();
    let federation = read_federation_persist_image(connection).unwrap().unwrap();
    connection.execute("INSERT INTO app_state(key,value_json) VALUES(?1,?2)",
        rusqlite::params![SQLITE_METADATA_KEY, &legacy]).unwrap();
    let units = read_metadata_units_unchecked(connection).unwrap();
    let generation = metadata_unit_generation(connection).unwrap();
    for version in [Some("1"), Some("3"), Some("2-metadata-units-3"), Some(""), None] {
        match version {
            Some(version) => { connection.execute(
                "UPDATE meta SET value=?1 WHERE key='schema_version'", [version]).unwrap(); }
            None => { connection.execute("DELETE FROM meta WHERE key='schema_version'", []).unwrap(); }
        }
        let changes = connection.total_changes();
        // This target deliberately answers false without reading metadata.
        // Its caller only retains an unmatched fence after a completed write;
        // false grants no write authority. Exercise a real metadata-reading
        // late fence below, and keep this non-reading path silent and negative.
        assert!(!PersistFenceTarget::TestRunCardsEpoch("current-proof".into())
            .is_already_durable(connection).unwrap());
        assert_eq!(connection.total_changes(), changes,
            "the false epoch answer must not write to an unknown-version store");
        let errors = [
            read_persisted_metadata_json(connection).unwrap_err(),
            read_persisted_metadata_field(connection, "testRunCardsEpoch").unwrap_err(),
            read_metadata_for_fence(connection, &["testRunCardsEpoch"]).err().unwrap(),
            read_federation_persist_image(connection).unwrap_err(),
            PersistFenceTarget::Federation(federation.clone())
                .is_already_durable(connection).unwrap_err(),
            ensure_sqlite_state_schema(connection).unwrap_err(),
            ensure_sqlite_state_schema_for_load(connection).err().unwrap(),
        ];
        for error in errors {
            assert!(format!("{error:#}").contains("unsupported state database schema"),
                "{version:?}: {error:#}");
        }
        assert_eq!(connection.total_changes(), changes,
            "neither false nor a refused readback authorizes a following write");
        assert_eq!(read_metadata_units_unchecked(connection).unwrap(), units);
        assert_eq!(metadata_unit_generation(connection).unwrap(), generation);
        let stored: String = connection.query_row(
            "SELECT value_json FROM app_state WHERE key=?1", [SQLITE_METADATA_KEY],
            |row| row.get(0)).unwrap();
        assert_eq!(stored, legacy);
        let stored_version: Option<String> = connection.query_row(
            "SELECT value FROM meta WHERE key='schema_version'", [], |row| row.get(0))
            .optional().unwrap();
        assert_eq!(stored_version.as_deref(), version);
    }
}

#[test]
fn metadata_units_remote_proxy_ids_never_reserve_local_identity_space() {
    for foreign_id in [
        "message-18446744073709551614",
        "message-18446744073709551615",
        "message-99999999999999999999",
        "message-peer-owned",
    ] {
        let mut fixture = FenceFixture::new();
        let id = metadata_units_session(&fixture);
        {
            let mut inner = fixture.inner.lock().unwrap();
            metadata_units_message(&mut inner, &id, "remote body");
            let index = inner.find_session_index(&id).unwrap();
            let record = inner.session_mut_by_index(index).unwrap();
            record.remote_id = Some("remote-peer".into());
            record.remote_session_id = Some("peer-session".into());
            if let Message::Text { id, .. } = &mut record.session.messages[0] {
                *id = foreign_id.into();
            }
        }
        let delta = fixture.collect();
        let expected_message_number = delta.metadata.next_message_number;
        let expected_session_number = delta.metadata.next_session_number;
        fixture.write(&delta).unwrap();
        let connection = metadata_units_connection(&fixture);
        let persisted = metadata_units_metadata(connection);
        assert_eq!(
            persisted.next_message_number, expected_message_number,
            "{foreign_id}"
        );
        assert_eq!(persisted.next_session_number, expected_session_number);
        let stored_id: String = connection
            .query_row(
                "SELECT message_id FROM messages WHERE session_id=?1 AND position=0",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            stored_id, foreign_id,
            "foreign content must still be durable"
        );
        let mut restored = load_state(&fixture.root.path().join("termal.sqlite"))
            .unwrap()
            .unwrap();
        assert_eq!(restored.next_message_number, expected_message_number);
        assert_eq!(
            restored.next_message_id(),
            format!("message-{expected_message_number}")
        );
    }
}

#[test]
fn metadata_units_migration_refuses_sequence_shaped_struct_before_committing_marker() {
    for source_version in ["2", "2-metadata-units-1"] {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        if source_version == "2" {
            metadata_units_seed_legacy(
                &connection,
                &PersistedState::metadata_from_inner(&StateInner::new()),
            );
            connection.execute("UPDATE app_state SET value_json=json_set(value_json,'$.codex',json('[null,[]]'))", []).unwrap();
        } else {
            metadata_units_seed_units1(&connection);
            connection
                .execute_batch(
                    "INSERT INTO state_metadata_units VALUES
                ('codex','','array','null'),
                ('codex','/00000000000000000000','entry','null'),
                ('codex','/00000000000000000001','entry','[]');",
                )
                .unwrap();
        }
        let encoded = read_persisted_metadata_json(&connection).unwrap().unwrap();
        let _: PersistedState = serde_json::from_str(&encoded).unwrap();
        let before_units =
            (source_version != "2").then(|| read_metadata_units_unchecked(&connection).unwrap());
        let provenance: String = connection
            .query_row("SELECT value_json FROM app_state", [], |row| row.get(0))
            .unwrap();
        let error = migrate_metadata_units(&connection).unwrap_err();
        assert!(format!("{error:#}").contains("invalid metadata descriptor kind for `codex`"));
        assert_eq!(metadata_units_version(&connection), source_version);
        assert_eq!(
            connection
                .query_row("SELECT value_json FROM app_state", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            provenance
        );
        if let Some(before) = before_units {
            assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), before);
            assert_eq!(metadata_unit_generation(&connection).unwrap(), 7);
        } else {
            assert!(
                !sqlite_state_user_table_names(&connection)
                    .unwrap()
                    .contains("state_metadata_units")
            );
        }
    }
}

#[test]
fn metadata_units_stale_capture_stays_refused_after_cache_reset_and_retry() {
    let mut fixture = FenceFixture::new();
    fixture.write(&fixture.collect()).unwrap();
    let stale = fixture.collect();
    let mut newer = stale.metadata.metadata_only();
    // Internal synchronous bookkeeping need not advance the SSE revision.
    newer
        .ignored_discovered_codex_thread_ids
        .insert("same-revision".into());
    persist_state_from_persisted(&fixture.root.path().join("termal.sqlite"), &newer).unwrap();
    fixture.cache = SqlitePersistConnectionCache::new();
    for _ in 0..2 {
        let error = fixture.write(&stale).unwrap_err();
        assert!(format!("{error:#}").contains("stale"));
    }
    let connection =
        open_sqlite_state_connection_unconfigured(&fixture.root.path().join("termal.sqlite"))
            .unwrap();
    assert!(
        metadata_units_metadata(&connection)
            .ignored_discovered_codex_thread_ids
            .contains("same-revision")
    );
    // Blindly recapturing an unchanged source must not grant overwrite authority.
    assert!(fixture.write(&fixture.collect()).is_err());
    // A reload incorporates the committed synchronous change and its predecessor.
    *fixture.inner.lock().unwrap() = load_state(&fixture.root.path().join("termal.sqlite"))
        .unwrap()
        .unwrap();
    fixture.write(&fixture.collect()).unwrap();
}

#[test]
fn metadata_units_first_uncached_delta_cannot_follow_a_newer_synchronous_write() {
    let mut fixture = FenceFixture::new();
    let stale = fixture.collect();
    let mut newer = stale.metadata.metadata_only();
    newer
        .ignored_discovered_codex_thread_ids
        .insert("first-write".into());
    persist_state_from_persisted(&fixture.root.path().join("termal.sqlite"), &newer).unwrap();
    assert!(format!("{:#}", fixture.write(&stale).unwrap_err()).contains("stale"));
    let connection =
        open_sqlite_state_connection_unconfigured(&fixture.root.path().join("termal.sqlite"))
            .unwrap();
    assert!(
        metadata_units_metadata(&connection)
            .ignored_discovered_codex_thread_ids
            .contains("first-write")
    );
}

#[test]
fn metadata_units_cold_unknown_source_and_higher_revision_do_not_grant_authority() {
    let mut fixture = FenceFixture::new();
    fixture.wait_record("durable-existing");
    fixture.write(&fixture.collect()).unwrap();
    let path = fixture.root.path().join("termal.sqlite");
    let mut unknown = StateInner::new();
    unknown.revision = 1_000_000;
    let stale = unknown.collect_persist_delta(0);
    fixture.cache = SqlitePersistConnectionCache::new();
    assert!(fixture.write(&stale).is_err());
    assert_eq!(
        load_state(&path).unwrap().unwrap().delegation_waits[0].id,
        "durable-existing"
    );
}

#[test]
fn metadata_units_same_revision_synchronous_source_update_preserves_unrelated_units() {
    let mut fixture = FenceFixture::new();
    fixture.wait_record("unrelated");
    fixture.write(&fixture.collect()).unwrap();
    let before = fixture.inner.lock().unwrap().revision;
    {
        let mut inner = fixture.inner.lock().unwrap();
        inner
            .ignored_discovered_codex_thread_ids
            .insert("legitimate".into());
        persist_state(&fixture.root.path().join("termal.sqlite"), &inner).unwrap();
    }
    fixture.write(&fixture.collect()).unwrap();
    let metadata = metadata_units_metadata(metadata_units_connection(&fixture));
    assert_eq!(metadata.revision, before);
    assert_eq!(metadata.delegation_waits[0].id, "unrelated");
    assert!(
        metadata
            .ignored_discovered_codex_thread_ids
            .contains("legitimate")
    );
}

#[test]
fn metadata_units_post_commit_failure_never_authorizes_changed_retry_payload() {
    let mut fixture = FenceFixture::new();
    let expected = fixture.wait_record("committed-unverified");
    let waiter = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected.clone()));
    fixture.receive(false);
    let mut pending = fixture.collect();
    METADATA_COMMIT_INTEGRITY_FAULT.with(|fault| fault.set(true));
    assert!(fixture.write(&pending).is_err());
    assert!(matches!(
        poll(&waiter),
        Some(Err(PersistFenceError::WriteFailed(_)))
    ));
    assert!(fixture.cache.metadata_units.is_none());
    pending.metadata.delegation_waits.clear();
    pending.metadata.revision = 1_000_000;
    assert!(
        fixture.write(&pending).is_err(),
        "a changed pending payload cannot reuse COMMIT"
    );
    let retry = fixture.enqueue(PersistFenceTarget::WaitRegistration(expected));
    fixture.receive(false);
    fixture.write(&fixture.collect()).unwrap();
    assert_eq!(poll(&retry), Some(Ok(())));
    assert!(matches!(
        poll(&waiter),
        Some(Err(PersistFenceError::WriteFailed(_)))
    ));
}

fn metadata_units_seed_units1(connection: &rusqlite::Connection) {
    metadata_units_seed_legacy(
        connection,
        &PersistedState::metadata_from_inner(&StateInner::new()),
    );
    connection
        .execute_batch(SQLITE_METADATA_UNITS_SCHEMA)
        .unwrap();
    let raw: Value = serde_json::from_str(
        &connection
            .query_row("SELECT value_json FROM app_state", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    for ((field, key), (kind, mut encoded)) in metadata_value_to_units(raw).unwrap() {
        if key.is_empty() && (kind == "array" || kind == "object") {
            encoded = "null".into();
        }
        connection
            .execute(
                "INSERT INTO state_metadata_units VALUES(?1,?2,?3,?4)",
                rusqlite::params![field, key, kind, encoded],
            )
            .unwrap();
    }
    connection
        .execute(
            "UPDATE meta SET value='2-metadata-units-1' WHERE key='schema_version'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO meta VALUES('metadata_units_generation','7')",
            [],
        )
        .unwrap();
}

#[test]
fn metadata_units_units1_upgrade_preserves_defaults_and_old_reader_refuses() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    metadata_units_seed_units1(&connection);
    let old_guard = || -> Result<()> {
        // Frozen units-1 compatibility guard: rejection precedes maintenance.
        let version = required_state_meta_value(&connection, "schema_version")?;
        if version != "2-metadata-units-1" {
            return Err(anyhow!("unsupported state schema"));
        }
        Ok(())
    };
    old_guard().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM state_metadata_units WHERE field_name='delegationWaits'",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        0,
        "legacy omitted defaults are valid in the original units-1 migration"
    );
    ensure_sqlite_state_schema_for_load(&connection)
        .unwrap()
        .unwrap();
    assert_eq!(metadata_units_version(&connection), SQLITE_SCHEMA_VERSION);
    assert_eq!(connection.query_row("SELECT kind,value_json FROM state_metadata_units WHERE field_name='delegationWaits' AND item_key=''", [],
        |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).unwrap(), ("array".into(), "0".into()));
    let changes = connection.total_changes();
    assert!(old_guard().is_err());
    assert_eq!(connection.total_changes(), changes);
}

#[test]
fn metadata_units_units1_missing_required_descriptor_refuses_upgrade_without_mutation() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    metadata_units_seed_units1(&connection);
    connection
        .execute(
            "DELETE FROM state_metadata_units WHERE field_name='nextMessageNumber'",
            [],
        )
        .unwrap();
    let changes = connection.total_changes();
    assert!(ensure_sqlite_state_schema_for_load(&connection).is_err());
    assert_eq!(connection.total_changes(), changes);
    assert_eq!(metadata_units_version(&connection), "2-metadata-units-1");
}

#[test]
fn metadata_units_units1_upgrade_marker_failure_is_atomic() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    metadata_units_seed_units1(&connection);
    let before = read_metadata_units_unchecked(&connection).unwrap();
    connection
        .execute_batch(
            "CREATE TEMP TRIGGER stop_units_upgrade BEFORE UPDATE ON meta
        WHEN old.key='schema_version' BEGIN SELECT RAISE(ABORT,'stop metadata upgrade'); END;",
        )
        .unwrap();
    assert!(migrate_metadata_units(&connection).is_err());
    assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), before);
    assert_eq!(metadata_unit_generation(&connection).unwrap(), 7);
    assert_eq!(metadata_units_version(&connection), "2-metadata-units-1");
    connection
        .execute_batch("DROP TRIGGER stop_units_upgrade")
        .unwrap();
    ensure_sqlite_state_schema_for_load(&connection).unwrap();
}

#[test]
fn metadata_units_scoped_reader_checks_inventory_and_count() {
    let mut fixture = FenceFixture::new();
    fixture.wait_record("scoped");
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_connection(&fixture);
    connection
        .execute(
            "DELETE FROM state_metadata_units WHERE field_name='delegationWaits' AND item_key<>''",
            [],
        )
        .unwrap();
    assert!(read_persisted_metadata_field(connection, "delegationWaits").is_err());
    connection
        .execute(
            "DELETE FROM state_metadata_units WHERE field_name='testRunWaits'",
            [],
        )
        .unwrap();
    assert!(read_persisted_metadata_field(connection, "federation").is_err());
}

#[test]
fn metadata_units_unsupported_legacy_fields_are_refused_without_lossy_migration() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    metadata_units_seed_legacy(
        &connection,
        &PersistedState::metadata_from_inner(&StateInner::new()),
    );
    connection.execute("UPDATE app_state SET value_json=json_set(value_json,'$.futureSafetyLedger',json('[1]'))", []).unwrap();
    assert!(migrate_metadata_units(&connection).is_err());
    assert_eq!(metadata_units_version(&connection), "2");
    assert!(
        !sqlite_state_user_table_names(&connection)
            .unwrap()
            .contains("state_metadata_units")
    );
}

#[test]
fn metadata_units_descriptor_counts_reject_object_loss_and_noncanonical_array_keys() {
    let mut units = metadata_value_to_units(json!({"object":{"one":1},"ordered":[1]})).unwrap();
    units.remove(&("object".into(), "/one".into()));
    assert!(metadata_units_to_value(&units).is_err());
    let mut units = metadata_value_to_units(json!({"ordered":[1]})).unwrap();
    let value = units
        .remove(&("ordered".into(), "/00000000000000000000".into()))
        .unwrap();
    units.insert(("ordered".into(), "/0".into()), value);
    assert!(metadata_units_to_value(&units).is_err());
}

#[test]
fn metadata_units_field_diagnostics_are_bounded_and_distinguish_rollback() {
    let mut fixture = FenceFixture::new();
    fixture.write(&fixture.collect()).unwrap();
    let observer = Arc::new(PersistObserver::default());
    let scope = observer.begin_at(std::time::Instant::now());
    fixture.inner.lock().unwrap().next_message_number += 1;
    fixture.write(&fixture.collect()).unwrap();
    scope.finish(true, &PersistTickTimings::default());
    metadata_units_connection(&fixture)
        .execute_batch(
            "CREATE TEMP TRIGGER stop_counter_diagnostic BEFORE UPDATE ON state_metadata_units
         WHEN new.field_name='nextMessageNumber' BEGIN SELECT RAISE(ABORT,'counter fault'); END;",
        )
        .unwrap();
    let scope = observer.begin_at(std::time::Instant::now());
    fixture.inner.lock().unwrap().next_message_number += 1;
    assert!(fixture.write(&fixture.collect()).is_err());
    scope.finish(false, &PersistTickTimings::default());
    let snapshot = observer.snapshot_at(std::time::Instant::now());
    assert_eq!(snapshot.recent.len(), 2);
    for tick in &snapshot.recent {
        assert_eq!(
            tick.counts.metadata_fields.len(),
            METADATA_UNIT_FIELDS.len()
        );
        assert!(
            tick.counts
                .metadata_fields
                .iter()
                .all(|row| METADATA_UNIT_FIELDS.contains(&row.field))
        );
        let field = tick
            .counts
            .metadata_fields
            .iter()
            .find(|row| row.field == "nextMessageNumber")
            .unwrap();
        assert_eq!(field.attempted_upserts, 1);
        assert!(field.attempted_bytes > 0);
        assert_eq!(field.committed_upserts, usize::from(tick.success));
        assert_eq!(tick.counts.metadata_transaction_committed, tick.success);
    }
}

#[test]
fn metadata_units_rollback_refuses_incomplete_or_orphan_authority_before_drop() {
    for fault in [
        "DELETE FROM state_metadata_units WHERE field_name='delegationWaits' AND item_key='/00000000000000000000'",
        "DELETE FROM state_metadata_units WHERE field_name='delegationWaits' AND item_key='/00000000000000000001'",
        "DELETE FROM state_metadata_units WHERE field_name='delegationWaits'",
        "DELETE FROM state_metadata_units WHERE field_name='delegationWaits' AND item_key=''",
        "UPDATE state_metadata_units SET item_key='/00000000000000000002' WHERE field_name='delegationWaits' AND item_key='/00000000000000000001'",
        "INSERT INTO state_metadata_units VALUES('unknown','','value','1')",
        "INSERT INTO state_metadata_units VALUES('revision','/orphan','entry','1')",
        "UPDATE state_metadata_units SET value_json='true' WHERE field_name='nextMessageNumber'",
        "UPDATE state_metadata_units SET value_json='\"3\"' WHERE field_name='nextMessageNumber'",
        "UPDATE state_metadata_units SET value_json='18446744073709551616' WHERE field_name='nextMessageNumber'",
    ] {
        let mut fixture = FenceFixture::new();
        fixture.wait_record("first");
        fixture.wait_record("second");
        fixture.write(&fixture.collect()).unwrap();
        let connection = metadata_units_rollback_connection(&fixture);
        connection.execute_batch(fault).unwrap();
        let before = read_metadata_units_unchecked(connection).unwrap();
        assert!(
            connection
                .execute_batch(&metadata_units_rollback_sql())
                .is_err(),
            "{fault}"
        );
        if !connection.is_autocommit() {
            connection.execute_batch("ROLLBACK").unwrap();
        }
        assert_eq!(metadata_units_version(connection), SQLITE_METADATA_UNITS_VERSION);
        assert_eq!(read_metadata_units_unchecked(connection).unwrap(), before);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM app_state", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn metadata_units_rollback_keeps_unsigned_counter_precision() {
    let mut fixture = FenceFixture::new();
    fixture.inner.lock().unwrap().next_message_number = u64::MAX - 1;
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_rollback_connection(&fixture);
    connection
        .execute_batch(&metadata_units_rollback_sql())
        .unwrap();
    let raw = metadata_units_old_reader(connection).unwrap();
    assert_eq!(raw["nextMessageNumber"].as_u64(), Some(u64::MAX - 1));
}

#[test]
fn metadata_units_rollback_preserves_non_nul_object_keys_without_prefix_collisions() {
    for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let mut fixture = FenceFixture::new();
        let path = fixture.root.path().join("termal.sqlite");
        {
            let connection = open_sqlite_state_connection_unconfigured(&path).unwrap();
            connection
                .pragma_update(None, "encoding", encoding)
                .unwrap();
            ensure_sqlite_state_schema(&connection).unwrap();
        }
        let expected: BTreeMap<String, String> = [
            ("", "empty"),
            ("a", "plain"),
            ("ab", "first suffix"),
            ("ac", "second suffix"),
            ("/leading", "leading"),
            ("żółw/🐢", "unicode"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        fixture
            .inner
            .lock()
            .unwrap()
            .pending_response_board_project_detachments = expected.clone();
        fixture.write(&fixture.collect()).unwrap();
        let connection = metadata_units_rollback_connection(&fixture);
        let actual_encoding: String = connection
            .pragma_query_value(None, "encoding", |row| row.get(0))
            .unwrap();
        assert_eq!(actual_encoding, encoding);
        assert_eq!(
            metadata_units_metadata(connection).pending_response_board_project_detachments,
            expected,
            "the keys are valid before rollback"
        );
        connection
            .execute_batch(&metadata_units_rollback_sql())
            .unwrap();
        let restored: PersistedState =
            serde_json::from_value(metadata_units_old_reader(connection).unwrap()).unwrap();
        assert_eq!(
            restored.pending_response_board_project_detachments, expected,
            "{encoding}"
        );
        ensure_sqlite_state_schema_for_load(connection).unwrap();
        assert_eq!(
            metadata_units_metadata(connection).pending_response_board_project_detachments,
            expected
        );
    }
}

#[test]
fn metadata_units_rollback_refuses_nul_object_keys_without_mutating_authority() {
    // Rollback deliberately refuses this valid units image: SQLite JSON
    // aggregation loses NUL suffixes. Refusal is the supported alternative
    // to silent loss; ordinary keys still have exact-preservation coverage.
    for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let mut fixture = FenceFixture::new();
        let path = fixture.root.path().join("termal.sqlite");
        {
            let connection = open_sqlite_state_connection_unconfigured(&path).unwrap();
            connection
                .pragma_update(None, "encoding", encoding)
                .unwrap();
            ensure_sqlite_state_schema(&connection).unwrap();
        }
        let expected: BTreeMap<String, String> = [
            ("", "empty"),
            ("a", "plain"),
            ("a\0b", "first suffix"),
            ("a\0c", "second suffix"),
            ("\0leading", "leading"),
            ("żółw/\0🐢", "unicode"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        fixture
            .inner
            .lock()
            .unwrap()
            .pending_response_board_project_detachments = expected.clone();
        fixture.write(&fixture.collect()).unwrap();
        let connection = metadata_units_rollback_connection(&fixture);
        let actual_encoding: String = connection
            .pragma_query_value(None, "encoding", |row| row.get(0))
            .unwrap();
        assert_eq!(actual_encoding, encoding);
        assert_eq!(
            metadata_units_metadata(connection).pending_response_board_project_detachments,
            expected
        );
        let before = read_metadata_units_unchecked(connection).unwrap();
        let generation = metadata_unit_generation(connection).unwrap();
        let error = connection
            .execute_batch(&metadata_units_rollback_sql())
            .unwrap_err();
        assert!(
            error.to_string().contains(
                "rollback refuses NUL object keys in pendingResponseBoardProjectDetachments"
            ),
            "{encoding}: {error}"
        );
        assert_eq!(read_metadata_units_unchecked(connection).unwrap(), before);
        assert_eq!(metadata_unit_generation(connection).unwrap(), generation);
        assert_eq!(metadata_units_version(connection), SQLITE_METADATA_UNITS_VERSION);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM app_state", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
        assert!(!connection.is_autocommit());
        connection.execute_batch("ROLLBACK").unwrap();
        assert_eq!(read_metadata_units_unchecked(connection).unwrap(), before);
        assert_eq!(metadata_unit_generation(connection).unwrap(), generation);
        assert_eq!(metadata_units_version(connection), SQLITE_METADATA_UNITS_VERSION);
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM app_state", [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn metadata_units_rollback_refuses_units1_without_touching_its_authority() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    metadata_units_seed_units1(&connection);
    let before = read_metadata_units_unchecked(&connection).unwrap();
    let provenance: String = connection
        .query_row("SELECT value_json FROM app_state", [], |row| row.get(0))
        .unwrap();
    assert!(
        connection
            .execute_batch(&metadata_units_rollback_sql())
            .is_err()
    );
    if !connection.is_autocommit() {
        connection.execute_batch("ROLLBACK").unwrap();
    }
    assert_eq!(metadata_units_version(&connection), "2-metadata-units-1");
    assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), before);
    assert_eq!(
        connection
            .query_row("SELECT value_json FROM app_state", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        provenance
    );
}

#[test]
fn metadata_units_rollback_late_marker_failure_restores_dropped_units_and_app_state() {
    let mut fixture = FenceFixture::new();
    fixture.wait_record("late-rollback-failure");
    fixture.write(&fixture.collect()).unwrap();
    let connection = metadata_units_rollback_connection(&fixture);
    let before = read_metadata_units_unchecked(connection).unwrap();
    let generation = metadata_unit_generation(connection).unwrap();
    connection
        .execute_batch(
            "CREATE TEMP TRIGGER stop_rollback_marker BEFORE UPDATE ON meta
        WHEN old.key='schema_version' AND new.value='2'
        BEGIN SELECT RAISE(ABORT,'injected late rollback failure'); END;",
        )
        .unwrap();
    let error = connection
        .execute_batch(&metadata_units_rollback_sql())
        .unwrap_err();
    assert!(error.to_string().contains("injected late rollback failure"));
    assert!(!connection.is_autocommit());
    connection.execute_batch("ROLLBACK").unwrap();
    assert_eq!(metadata_units_version(connection), SQLITE_METADATA_UNITS_VERSION);
    assert_eq!(read_metadata_units_unchecked(connection).unwrap(), before);
    assert_eq!(metadata_unit_generation(connection).unwrap(), generation);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM app_state", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
}
