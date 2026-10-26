// Disposable compatibility fixtures; no live store or older binary is opened.
// Shared with the metadata-unit rollback tests: sqlite3 CLI directives are
// not SQL and must be removed before executing the same script via rusqlite.
#[cfg(test)]
fn metadata_units_rollback_sql() -> String {
    include_str!("../scripts/state-metadata-units-rollback.sql")
        .lines()
        .filter(|line| !line.starts_with('.'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod ordinary_handoff_reader_floor_tests {
    use super::*;

    fn units_store() -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        seed_units_store(&connection);
        connection
    }

    fn seed_units_store(connection: &rusqlite::Connection) {
        seed_current_state_core_tables(&connection);
        seed_current_state_auxiliary_tables(&connection);
        seed_current_state_metadata(&connection);
        connection
            .execute(
                "INSERT INTO meta VALUES('metadata_units_generation','7')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE meta SET value=?1 WHERE key='schema_version'",
                [SQLITE_METADATA_UNITS_VERSION],
            )
            .unwrap();
        // Deliberately stale migration provenance is never current authority.
        connection
            .execute(
                "INSERT INTO app_state VALUES(?1,'{invalid provenance')",
                [SQLITE_METADATA_KEY],
            )
            .unwrap();
        connection.execute("INSERT INTO sessions(id,value_json) VALUES('retained',?1)",
            [r#"{"session":{},"engram_uncertain_grant_id":"possibly-applied","queued_prompts":[{"engram_evaluate":{"prepared_begin":{"phase":"prepared","grant_id":"possibly-applied","idempotency_key":"original-key","delivery_tokens":["original-page"]}}}]}"#]).unwrap();
    }

    fn legacy_store(source: &str) -> rusqlite::Connection {
        if source == "2-metadata-units-1" {
            let connection = units_store();
            connection
                .execute(
                    "UPDATE meta SET value=?1 WHERE key='schema_version'",
                    [source],
                )
                .unwrap();
            connection.execute("UPDATE state_metadata_units SET value_json='null' WHERE item_key='' AND kind IN ('array','object')", []).unwrap();
            return connection;
        }
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        seed_primary2_state_schema_for_test(&connection);
        let metadata = PersistedState::metadata_from_inner(&StateInner::new());
        connection
            .execute(
                "INSERT INTO app_state VALUES(?1,?2)",
                rusqlite::params![
                    SQLITE_METADATA_KEY,
                    serde_json::to_string(&metadata).unwrap()
                ],
            )
            .unwrap();
        connection
    }

    fn version(connection: &rusqlite::Connection) -> String {
        required_state_meta_value(connection, "schema_version").unwrap()
    }

    fn prepare(connection: &rusqlite::Connection, load: bool) -> Result<()> {
        if load {
            ensure_sqlite_state_schema_for_load(connection).map(|_| ())
        } else {
            ensure_sqlite_state_schema(connection)
        }
    }

    fn image(
        connection: &rusqlite::Connection,
    ) -> (PersistMetadataUnits, u64, Vec<(String, String)>, String) {
        let mut statement = connection
            .prepare("SELECT id,value_json FROM sessions ORDER BY id")
            .unwrap();
        let sessions = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let provenance = connection
            .query_row(
                "SELECT value_json FROM app_state WHERE key=?1",
                [SQLITE_METADATA_KEY],
                |row| row.get(0),
            )
            .unwrap();
        (
            read_metadata_units_unchecked(connection).unwrap(),
            metadata_unit_generation(connection).unwrap(),
            sessions,
            provenance,
        )
    }

    #[test]
    fn admission_contract_reader_floor_preserves_units_and_uncertain_sessions() {
        for load in [false, true] {
            let connection = units_store();
            let before = image(&connection);
            let raw_federation = read_federation_persist_image(&connection).unwrap();
            prepare(&connection, load).unwrap();
            assert_eq!(version(&connection), SQLITE_SCHEMA_VERSION);
            assert_eq!(
                image(&connection),
                before,
                "only the semantic header changes"
            );
            assert_eq!(
                read_federation_persist_image(&connection).unwrap(),
                raw_federation
            );
            assert!(read_persisted_metadata_json(&connection).unwrap().is_some());
            prepare(&connection, load).unwrap();
            assert_eq!(image(&connection), before, "reopen is idempotent");
        }
    }

    #[test]
    fn admission_contract_reader_floor_keeps_both_legacy_normalization_paths() {
        for source in ["2", "2-metadata-units-1"] {
            for load in [false, true] {
                let connection = legacy_store(source);
                let before = read_persisted_metadata_json(&connection).unwrap().unwrap();
                prepare(&connection, load).unwrap();
                assert_eq!(version(&connection), SQLITE_SCHEMA_VERSION);
                let after = read_persisted_metadata_json(&connection).unwrap().unwrap();
                let before: PersistedState = serde_json::from_str(&before).unwrap();
                let after: PersistedState = serde_json::from_str(&after).unwrap();
                assert_eq!(
                    project_metadata_units(&before).unwrap(),
                    project_metadata_units(&after).unwrap()
                );
            }
        }
    }

    #[test]
    fn admission_contract_reader_floor_failed_second_stage_keeps_valid_units_predecessor() {
        for source in ["2", "2-metadata-units-1"] {
            for load in [false, true] {
                let connection = legacy_store(source);
                connection
                    .execute_batch(
                        "CREATE TRIGGER refuse_handoff_floor BEFORE UPDATE OF value ON meta
                    WHEN new.key='schema_version' AND new.value='2-metadata-units-2-handoff-1'
                    BEGIN SELECT RAISE(ABORT,'injected handoff floor failure'); END;",
                    )
                    .unwrap();
                let error = prepare(&connection, load).unwrap_err();
                assert!(format!("{error:#}").contains("injected handoff floor failure"));
                assert_eq!(version(&connection), SQLITE_METADATA_UNITS_VERSION);
                validate_sqlite_state_schema_for_version(
                    &connection,
                    SQLITE_METADATA_UNITS_VERSION,
                )
                .unwrap();
                validate_no_legacy_embedded_state(&connection).unwrap();
                let predecessor = read_metadata_units(&connection).unwrap();
                assert!(connection.is_autocommit());
                connection
                    .execute_batch("DROP TRIGGER refuse_handoff_floor")
                    .unwrap();
                prepare(&connection, load).unwrap();
                assert_eq!(version(&connection), SQLITE_SCHEMA_VERSION);
                assert_eq!(read_metadata_units(&connection).unwrap(), predecessor);
            }
        }
    }

    #[test]
    fn admission_contract_reader_floor_cas_requires_exactly_one_row() {
        let connection = units_store();
        let before = image(&connection);
        connection
            .execute_batch(
                "CREATE TRIGGER ignore_floor BEFORE UPDATE OF value ON meta
            WHEN new.key='schema_version' BEGIN SELECT RAISE(IGNORE); END;",
            )
            .unwrap();
        let error = advance_sqlite_state_reader_floor(&connection, true)
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("exact predecessor"));
        assert_eq!(version(&connection), SQLITE_METADATA_UNITS_VERSION);
        assert_eq!(image(&connection), before);
        assert!(connection.is_autocommit());
    }

    #[test]
    fn admission_contract_reader_floor_transaction_rechecks_current_and_unknown_headers() {
        for load in [false, true] {
            let connection = units_store();
            // Model another opener completing after the outer predecessor read.
            connection
                .execute(
                    "UPDATE meta SET value=?1 WHERE key='schema_version'",
                    [SQLITE_SCHEMA_VERSION],
                )
                .unwrap();
            let before = image(&connection);
            let changes = connection.total_changes();
            advance_sqlite_state_reader_floor(&connection, load).unwrap();
            assert_eq!(connection.total_changes(), changes);
            assert_eq!(image(&connection), before);
            connection
                .execute(
                    "UPDATE meta SET value='future' WHERE key='schema_version'",
                    [],
                )
                .unwrap();
            let changes = connection.total_changes();
            assert!(advance_sqlite_state_reader_floor(&connection, load).is_err());
            assert_eq!(connection.total_changes(), changes);
            assert_eq!(version(&connection), "future");
        }
    }

    #[test]
    fn admission_contract_reader_floor_rejects_corrupt_units_before_header_change() {
        for load in [false, true] {
            for corruption in [
                "DELETE FROM state_metadata_units WHERE field_name='projects'",
                "UPDATE state_metadata_units SET value_json='1' WHERE field_name='projects' AND item_key=''",
                "UPDATE state_metadata_units SET kind='value' WHERE field_name='projects' AND item_key=''",
                "INSERT INTO state_metadata_units VALUES('projects','/1','entry','{}')",
                "INSERT INTO state_metadata_units VALUES('futureSafetyLedger','','array','0')",
                "UPDATE state_metadata_units SET value_json='1' WHERE field_name='sessions' AND item_key=''",
            ] {
                let connection = units_store();
                connection.execute_batch(corruption).unwrap();
                let before = image(&connection);
                let changes = connection.total_changes();
                assert!(prepare(&connection, load).is_err(), "{corruption}");
                assert_eq!(version(&connection), SQLITE_METADATA_UNITS_VERSION);
                assert_eq!(image(&connection), before);
                assert_eq!(connection.total_changes(), changes);
            }
        }
    }

    #[test]
    fn admission_contract_reader_floor_rejects_unknown_formats_before_mutation() {
        for load in [false, true] {
            for unknown in ["1", "3", "4", "future", "2-metadata-units-2-handoff-2"] {
                let connection = units_store();
                connection
                    .execute(
                        "UPDATE meta SET value=?1 WHERE key='schema_version'",
                        [unknown],
                    )
                    .unwrap();
                let before = image(&connection);
                let changes = connection.total_changes();
                assert!(prepare(&connection, load).is_err());
                assert!(read_persisted_metadata_json(&connection).is_err());
                assert!(read_persisted_metadata_field(&connection, "projects").is_err());
                assert_eq!(image(&connection), before);
                assert_eq!(connection.total_changes(), changes);
                assert_eq!(version(&connection), unknown);
            }
        }
    }

    #[test]
    fn admission_contract_old_units_reader_guard_refuses_handoff_floor() {
        let connection = units_store();
        prepare(&connection, true).unwrap();
        let before = image(&connection);
        let changes = connection.total_changes();
        // Frozen units-2 validator oracle, not execution of an older binary.
        // Both its load and write-only paths called this before maintenance.
        assert!(
            validate_sqlite_state_schema_for_version(&connection, SQLITE_METADATA_UNITS_VERSION)
                .is_err()
        );
        assert_eq!(connection.total_changes(), changes);
        assert_eq!(image(&connection), before);
    }

    #[test]
    fn admission_contract_reader_floor_initializes_genuinely_empty_store() {
        for load in [false, true] {
            let connection = rusqlite::Connection::open_in_memory().unwrap();
            prepare(&connection, load).unwrap();
            assert_eq!(version(&connection), SQLITE_SCHEMA_VERSION);
            assert!(read_persisted_metadata_json(&connection).unwrap().is_none());
            prepare(&connection, load).unwrap();
            assert_eq!(version(&connection), SQLITE_SCHEMA_VERSION);
        }
    }

    #[test]
    fn admission_contract_reader_floor_reopens_committed_normalization_after_floor_failure() {
        let root = TestTempRoot::create("handoff-interstage-reopen");
        let path = root.path().join("state.sqlite");
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            seed_primary2_state_schema_for_test(&connection);
            let metadata = PersistedState::metadata_from_inner(&StateInner::new());
            connection
                .execute(
                    "INSERT INTO app_state VALUES(?1,?2)",
                    rusqlite::params![
                        SQLITE_METADATA_KEY,
                        serde_json::to_string(&metadata).unwrap()
                    ],
                )
                .unwrap();
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_feature BEFORE UPDATE OF value ON meta
                WHEN new.value='2-metadata-units-2-handoff-1'
                BEGIN SELECT RAISE(ABORT,'feature floor unavailable'); END;",
                )
                .unwrap();
            assert!(prepare(&connection, true).is_err());
            assert_eq!(version(&connection), SQLITE_METADATA_UNITS_VERSION);
        }
        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(version(&connection), SQLITE_METADATA_UNITS_VERSION);
        let before = image(&connection);
        validate_no_legacy_embedded_state(&connection).unwrap();
        connection
            .execute_batch("DROP TRIGGER fail_feature")
            .unwrap();
        prepare(&connection, true).unwrap();
        assert_eq!(version(&connection), SQLITE_SCHEMA_VERSION);
        assert_eq!(image(&connection), before);
    }

    #[test]
    fn admission_contract_units_rollback_cannot_remove_handoff_floor() {
        let connection = units_store();
        prepare(&connection, true).unwrap();
        let before = image(&connection);
        let error = connection
            .execute_batch(&metadata_units_rollback_sql())
            .unwrap_err();
        assert!(
            matches!(&error, rusqlite::Error::SqliteFailure(code, Some(message))
            if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_CHECK
                && message == "CHECK constraint failed: value=1"),
            "the reader-floor CHECK must refuse before rollback, not a SQL parser error: {error}"
        );
        if !connection.is_autocommit() {
            connection.execute_batch("ROLLBACK").unwrap();
        }
        assert_eq!(version(&connection), SQLITE_SCHEMA_VERSION);
        assert_eq!(image(&connection), before);
    }

    #[test]
    fn admission_contract_units_rollback_supported_floor_executes_same_sql() {
        let connection = units_store();
        let before = read_metadata_units(&connection).unwrap();
        connection
            .execute_batch(&metadata_units_rollback_sql())
            .expect("the supported units floor must execute the rollback rather than fail parsing");
        assert!(connection.is_autocommit());
        assert_eq!(version(&connection), "2");
        let encoded: String = connection
            .query_row(
                "SELECT value_json FROM app_state WHERE key=?1",
                [SQLITE_METADATA_KEY],
                |row| row.get(0),
            )
            .unwrap();
        let restored: PersistedState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(project_metadata_units(&restored).unwrap(), before);
    }

    #[test]
    fn admission_contract_failed_floor_never_persists_a_marker_bearing_session() {
        let root = TestTempRoot::create("handoff-floor-write-barrier");
        let path = root.path().join("state.sqlite");
        let connection = rusqlite::Connection::open(&path).unwrap();
        seed_units_store(&connection);
        connection
            .execute_batch(
                "CREATE TRIGGER fail_feature BEFORE UPDATE OF value ON meta
            WHEN new.value='2-metadata-units-2-handoff-1'
            BEGIN SELECT RAISE(ABORT,'feature floor unavailable'); END;",
            )
            .unwrap();
        let before = image(&connection);
        let mut inner = StateInner::new();
        let record = inner.create_session(
            Agent::Codex,
            Some("floor barrier".into()),
            root.path().to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        let record = inner.session_mut(&session_id).unwrap();
        record.queued_prompts.push_back(QueuedPromptRecord {
            engram_ordinary_handoff: true,
            engram_waiting: false,
            promoted_message_index: None,
            promotion_disposition_known: true,
            engram_bind: None,
            engram_evaluate: None,
            engram_interrupted: true,
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                engram_interrupted: true,
                is_engram_retained: true,
                attachments: Vec::new(),
                id: "marker-head".into(),
                timestamp: stamp_now(),
                text: "ungranted handoff".into(),
                expanded_text: None,
                source: None,
            },
        });
        let persisted = PersistedState::from_inner(&inner);
        assert!(
            persisted.sessions.iter().any(|record| {
                record.session.id == session_id
                    && record
                        .queued_prompts
                        .front()
                        .is_some_and(|head| head.engram_ordinary_handoff)
            }),
            "the production snapshot must contain the marked head"
        );
        let error = persist_persisted_state_to_sqlite(&path, &persisted).unwrap_err();
        assert!(format!("{error:#}").contains("feature floor unavailable"));
        assert_eq!(version(&connection), SQLITE_METADATA_UNITS_VERSION);
        assert_eq!(image(&connection), before);
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
                [&session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!present, "marker cannot precede the confirmed reader floor");
    }
}
