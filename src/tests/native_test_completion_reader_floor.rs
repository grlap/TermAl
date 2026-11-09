// Disposable MAIN-store migrations through the actual admission/writer owners.
// No live store, provider execution, verification credit or older binary runs.
use super::*;

#[path = "direct_prompt_reader_floor.rs"]
mod direct_prompt;

const OLD_FLOORS: &[&str] = &["2", "2-metadata-units-1", SQLITE_METADATA_UNITS_VERSION,
    SQLITE_OBSERVATION_PREDECESSOR, SQLITE_OBSERVATION_SCHEMA_VERSION,
    SQLITE_CHECKPOINT_RESULT_PREDECESSOR, SQLITE_NATIVE_TEST_COMPLETION_PREDECESSOR,
    SQLITE_NATIVE_TEST_COMPLETION_IMPORT_PREDECESSOR];

fn import_record() -> EngramImportReconciliationRecord {
    EngramImportReconciliationRecord::LocalImportFence(EngramImportLocalFence {
        schema_version: ENGRAM_IMPORT_RECONCILIATION_RECORD_VERSION,
        record_id: Uuid::new_v4().to_string(),
        scope: EngramImportReconciliationScope {
            store_association: "fixture-store".to_owned(), project_id: "example.test/native-floor".to_owned(),
            association_id: Uuid::new_v4().to_string(), owner_id: None,
        },
        hold_id: Uuid::new_v4().to_string(), candidate_id: Uuid::new_v4().to_string(),
        candidate_revision: 1, scope_revision: 1, hold_revision: 1,
        affected_scope_ref: "fixture-scope".to_owned(), host_incarnation: Uuid::new_v4().to_string(),
        fence_generation: 1, phase: EngramImportFencePhase::Installing,
        prepared_manifest_ref: None, attestation_ref: None, terminal_decision_ref: None,
    })
}

fn seed_source(connection: &rusqlite::Connection, version: &str, nonempty_import: bool) {
    ensure_sqlite_state_schema(connection).unwrap();
    let mut units = read_metadata_units_unchecked(connection).unwrap();
    units.retain(|(field, _), _| field != NATIVE_TEST_FACTS_FIELD);
    if !matches!(version, SQLITE_CHECKPOINT_RESULT_PREDECESSOR | SQLITE_NATIVE_TEST_COMPLETION_PREDECESSOR
        | SQLITE_NATIVE_TEST_COMPLETION_IMPORT_PREDECESSOR) {
        units.retain(|(field, _), _| field != "engramSessionObligations");
    }
    if version == SQLITE_NATIVE_TEST_COMPLETION_IMPORT_PREDECESSOR {
        let mut journal = EngramImportReconciliationJournal::activated_empty();
        if nonempty_import { journal.record(import_record()).unwrap(); }
        units.extend(engram_import_reconciliation_units(&journal).unwrap());
    }
    connection.execute("DELETE FROM state_metadata_units", []).unwrap();
    if version == "2" {
        let value = metadata_units_to_value(&units).unwrap();
        connection.execute_batch("DROP TABLE state_metadata_units; DELETE FROM meta WHERE key='metadata_units_generation';").unwrap();
        connection.execute("INSERT INTO app_state(key,value_json) VALUES(?1,?2)",
            rusqlite::params![SQLITE_METADATA_KEY, value.to_string()]).unwrap();
    } else {
        for ((field, key), (kind, encoded)) in units {
            let encoded = if version == "2-metadata-units-1" && key.is_empty() && matches!(kind.as_str(), "array" | "object") {
                "null".to_owned()
            } else { encoded };
            connection.execute("INSERT INTO state_metadata_units(field_name,item_key,kind,value_json) VALUES(?1,?2,?3,?4)",
                rusqlite::params![field,key,kind,encoded]).unwrap();
        }
        connection.execute("UPDATE meta SET value='7' WHERE key='metadata_units_generation'", []).unwrap();
    }
    connection.execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [version]).unwrap();
}

fn source(version: &str, nonempty_import: bool) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    seed_source(&connection, version, nonempty_import);
    connection
}

fn image(connection: &rusqlite::Connection) -> Vec<(String, String, Vec<Vec<rusqlite::types::Value>>)> {
    let tables = connection.prepare("SELECT name,sql FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).unwrap()
        .collect::<rusqlite::Result<Vec<_>>>().unwrap();
    tables.into_iter().map(|(name, schema)| {
        let table = format!("\"{}\"", name.replace('"', "\"\""));
        let columns = connection.prepare(&format!("SELECT * FROM {table}")).unwrap().column_count();
        let order = (1..=columns).map(|column| column.to_string()).collect::<Vec<_>>().join(",");
        let sql = format!("SELECT * FROM {table} ORDER BY {order}");
        let mut statement = connection.prepare(&sql).unwrap();
        let rows = statement.query_map([], |row| (0..columns).map(|column| row.get(column)).collect::<rusqlite::Result<Vec<rusqlite::types::Value>>>()).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap();
        (name, schema, rows)
    }).collect()
}

#[test]
fn native_floor_migrates_ordinary_and_activated_sources_without_rewriting_raw_custody() {
    for activated in [false, true] {
        for nonempty in [false, true] {
            let version = if activated { SQLITE_NATIVE_TEST_COMPLETION_IMPORT_PREDECESSOR } else { SQLITE_NATIVE_TEST_COMPLETION_PREDECESSOR };
            let connection = source(version, nonempty);
            // A genuinely old registration has no native custody. Whitespace
            // in this retained raw row must not be normalized by migration.
            let id = format!("test-{}", Uuid::new_v4());
            let registration: TestRunRegistration = serde_json::from_value(json!({
                "runId":id,"projectId":null,"runDirectory":"/fixture/old-run","worktree":"/fixture",
                "requestIdentity":sha256_hex(Uuid::new_v4().as_bytes()),"initiatingSessionId":"session-old",
                "owningSessionId":"session-old","creatorPid":null,"registeredAt":"old","startedAt":null,
                "preset":"full","detached":true,"stageNames":[],"workBinding":null,"validatedUnbound":true
            })).unwrap();
            let encoded = format!(" {} \n", serde_json::to_string(&registration).unwrap());
            connection.execute("INSERT INTO state_metadata_units VALUES('testRunRegistrations',?1,'entry',?2)",
                rusqlite::params![format!("/{id}"),encoded]).unwrap();
            connection.execute("UPDATE state_metadata_units SET value_json='1' WHERE field_name='testRunRegistrations' AND item_key=''", []).unwrap();
            let before = read_metadata_units_unchecked(&connection).unwrap();
            let generation = metadata_unit_generation(&connection).unwrap();
            let other_tables = image(&connection).into_iter().filter(|(name, _, _)| name != "meta" && name != "state_metadata_units").collect::<Vec<_>>();
            let loaded = prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap().unwrap();
            assert!(loaded.remote_test_run_facts.is_empty());
            assert!(loaded.test_run_registrations[&id].native_origin.is_none());
            assert!(loaded.test_run_registrations[&id].native_completion.is_none());
            let mut expected = before;
            expected.insert((NATIVE_TEST_FACTS_FIELD.to_owned(), String::new()), ("object".to_owned(), "0".to_owned()));
            assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), expected);
            assert_eq!(metadata_unit_generation(&connection).unwrap(), generation + 1);
            assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(),
                if activated { SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION } else { SQLITE_SCHEMA_VERSION });
            assert_eq!(image(&connection).into_iter().filter(|(name, _, _)| name != "meta" && name != "state_metadata_units").collect::<Vec<_>>(), other_tables);
            assert_eq!(matches!(read_engram_import_reconciliation_state(&connection).unwrap(), EngramImportReconciliationState::Activated(_)), activated);
            let settled = image(&connection);
            prepare_existing_sqlite_state_reader_floor(&connection, false).unwrap();
            assert_eq!(image(&connection), settled, "reopening is idempotent");
            assert!(validate_sqlite_state_schema_for_version(&connection, version).is_err(), "frozen old reader refuses before typed load");
        }
    }
}

#[test]
fn native_floor_preserves_every_supported_earlier_ordered_migration() {
    for version in OLD_FLOORS {
        let connection = source(version, false);
        let loaded = prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap().unwrap();
        assert!(loaded.remote_test_run_facts.is_empty(), "{version}");
        let activated = *version == SQLITE_NATIVE_TEST_COMPLETION_IMPORT_PREDECESSOR;
        assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(),
            if activated { SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION } else { SQLITE_SCHEMA_VERSION });
        let before = image(&connection);
        prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap();
        assert_eq!(image(&connection), before);
    }
}

#[test]
fn native_floor_rejects_target_only_keys_in_all_old_formats_before_normalization() {
    for version in OLD_FLOORS {
        for field in ["remoteTestRunFacts", "testRunRegistrations", "testRunCompacted", "testRunWaits"] {
            let connection = source(version, false);
            let row = match field {
                "testRunRegistrations" => json!({"nativeOrigin":null}),
                "testRunCompacted" => json!({"registration":{"nativeCompletion":{}}}),
                "testRunWaits" => json!({"runs":[{"remote":null}]}),
                _ => Value::Null,
            };
            if *version == "2" {
                let encoded: String = connection.query_row("SELECT value_json FROM app_state WHERE key=?1", [SQLITE_METADATA_KEY], |row| row.get(0)).unwrap();
                let mut value: Value = serde_json::from_str(&encoded).unwrap();
                value[field] = if field == "testRunWaits" { json!([row]) }
                    else if field == "remoteTestRunFacts" { row } else { json!({"injected":row}) };
                connection.execute("UPDATE app_state SET value_json=?2 WHERE key=?1", rusqlite::params![SQLITE_METADATA_KEY,value.to_string()]).unwrap();
            } else if field == "remoteTestRunFacts" {
                connection.execute("INSERT INTO state_metadata_units VALUES(?1,'','object','0')", [field]).unwrap();
            } else {
                let key = if field == "testRunWaits" { "/00000000000000000000" } else { "/injected" };
                connection.execute("INSERT INTO state_metadata_units VALUES(?1,?2,'entry',?3)", rusqlite::params![field,key,row.to_string()]).unwrap();
                if *version != "2-metadata-units-1" {
                    connection.execute("UPDATE state_metadata_units SET value_json='1' WHERE field_name=?1 AND item_key=''", [field]).unwrap();
                }
            }
            let before = image(&connection);
            let error = prepare_existing_sqlite_state_reader_floor(&connection, true).err().unwrap();
            assert!(error.to_string().contains("native test completion custody"), "{version}/{field}: {error:#}");
            assert_eq!(image(&connection), before, "{version}/{field}");
        }
    }
}

#[test]
fn native_floor_rolls_back_descriptor_generation_header_and_older_normalization_together() {
    for version in ["2", SQLITE_NATIVE_TEST_COMPLETION_PREDECESSOR, SQLITE_NATIVE_TEST_COMPLETION_IMPORT_PREDECESSOR] {
        for phase in ["validated", "descriptor", "header", "readback"] {
            let connection = source(version, true);
            let before = image(&connection);
            TEST_NATIVE_TEST_FLOOR_FAULT.with(|fault| fault.set(Some(phase)));
            let result = prepare_existing_sqlite_state_reader_floor(&connection, true);
            TEST_NATIVE_TEST_FLOOR_FAULT.with(|fault| fault.set(None));
            assert!(result.is_err(), "{version}/{phase}");
            assert_eq!(image(&connection), before, "{version}/{phase}");
            assert!(connection.is_autocommit());
        }
    }
}

#[test]
fn native_floor_invalid_predecessor_units_keep_reset_guidance_without_writes() {
    for version in OLD_FLOORS.iter().copied().filter(|version| *version != "2") {
        for for_load in [false, true] {
            for field in ["remoteTestRunFacts", "testRunRegistrations", "testRunCompacted", "testRunWaits"] {
                let connection = source(version, true);
                connection.execute(
                    "INSERT INTO state_metadata_units(field_name,item_key,kind,value_json) VALUES(?1,'','object','{broken json')
                     ON CONFLICT(field_name,item_key) DO UPDATE SET value_json=excluded.value_json",
                    [field],
                ).unwrap();
                let before = image(&connection);
                let changes = connection.total_changes();
                let error = prepare_existing_sqlite_state_reader_floor(&connection, for_load).err().unwrap();
                let diagnostic = format!("{error:#}");
                assert!(diagnostic.contains("unsupported state database schema"), "{version}/{field}: {diagnostic}");
                assert!(diagnostic.contains("Move or delete `termal.sqlite`"), "{version}/{field}: {diagnostic}");
                assert_eq!(connection.total_changes(), changes, "{version}/{field}");
                assert_eq!(image(&connection), before, "header, generation and raw custody: {version}/{field}");
                assert!(connection.is_autocommit());
            }
            let connection = source(version, true);
            connection.execute_batch("DROP TABLE state_metadata_units").unwrap();
            let before = image(&connection);
            let changes = connection.total_changes();
            let error = prepare_existing_sqlite_state_reader_floor(&connection, for_load).err().unwrap();
            let diagnostic = format!("{error:#}");
            assert!(diagnostic.contains("unsupported state database schema"), "{version}: {diagnostic}");
            assert!(diagnostic.contains("Move or delete `termal.sqlite`"), "{version}: {diagnostic}");
            assert_eq!(connection.total_changes(), changes, "{version}");
            assert_eq!(image(&connection), before, "missing table stays missing: {version}");
            assert!(connection.is_autocommit());
        }
    }
}

#[test]
fn native_floor_current_missing_malformed_and_unknown_metadata_stay_corruption() {
    for activated in [false, true] {
        for sql in [
            "DELETE FROM state_metadata_units WHERE field_name='remoteTestRunFacts'",
            "UPDATE state_metadata_units SET kind='array' WHERE field_name='remoteTestRunFacts'",
            "UPDATE state_metadata_units SET value_json='1' WHERE field_name='remoteTestRunFacts'",
            "INSERT INTO state_metadata_units VALUES('futureNativeFacts','','object','0')",
        ] {
            let connection = if activated {
                let connection = source(SQLITE_NATIVE_TEST_COMPLETION_IMPORT_PREDECESSOR, true);
                prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap();
                connection
            } else {
                let connection = rusqlite::Connection::open_in_memory().unwrap();
                ensure_sqlite_state_schema(&connection).unwrap();
                connection
            };
            connection.execute(sql, []).unwrap();
            let before = image(&connection);
            assert!(prepare_existing_sqlite_state_reader_floor(&connection, true).is_err());
            assert_eq!(image(&connection), before);
        }
    }
}

#[test]
fn native_floor_stale_source_capture_cannot_erase_migrated_custody() {
    let connection = source(SQLITE_NATIVE_TEST_COMPLETION_PREDECESSOR, false);
    let inner = StateInner::new();
    let generation = metadata_unit_generation(&connection).unwrap();
    inner.metadata_predecessor.store(generation, Ordering::Release);
    let stale = PersistedState::metadata_from_inner(&inner);
    stale.metadata_capture.verify(generation).unwrap();
    prepare_existing_sqlite_state_reader_floor(&connection, false).unwrap();
    let before = image(&connection);
    let error = write_metadata_units(&connection, project_metadata_units(&stale).unwrap(), None, &[], Some(&stale.metadata_capture), false).err().unwrap();
    assert!(error.downcast_ref::<PersistMetadataSnapshotStale>().is_some());
    assert_eq!(image(&connection), before);
}

#[test]
fn native_floor_concurrent_openers_commit_one_extension_and_keep_import_activation_explicit() {
    let temporary = TestTempRoot::create("native-floor");
    let path = temporary.path().join("state.sqlite");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        seed_source(
            &connection,
            SQLITE_NATIVE_TEST_COMPLETION_PREDECESSOR,
            false,
        );
    }
    crate::tests::phase_sync::run_concurrently((0..2).map(|index| {
        let path = path.clone();
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.busy_timeout(SQLITE_BUSY_TIMEOUT).unwrap();
        move || {
            prepare_existing_sqlite_state_reader_floor(&connection, index == 0)
                .map(|_| ())
                .unwrap();
        }
    }));
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(metadata_unit_generation(&connection).unwrap(), 8);
    assert!(matches!(
        read_engram_import_reconciliation_state(&connection).unwrap(),
        EngramImportReconciliationState::NotActivated
    ));
    let before = image(&connection);
    prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap();
    assert_eq!(
        image(&connection),
        before,
        "ordinary reopen never activates import"
    );
    let acknowledged = activate_engram_import_reconciliation_sqlite(
        &path,
        &connection,
        &EngramImportActivationCapability::for_test(),
        import_record(),
        |journal, _| journal.clone(),
    )
    .unwrap();
    assert_eq!(
        read_engram_import_reconciliation_state(&connection).unwrap(),
        EngramImportReconciliationState::Activated(Arc::new(acknowledged))
    );
    assert_eq!(
        required_state_meta_value(&connection, "schema_version").unwrap(),
        SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
    );
    let before = image(&connection);
    assert!(
        activate_engram_import_reconciliation_sqlite(
            &path,
            &connection,
            &EngramImportActivationCapability::for_test(),
            import_record(),
            |_, _| ()
        )
        .is_err()
    );
    assert_eq!(image(&connection), before, "activation is not replayed");
}
