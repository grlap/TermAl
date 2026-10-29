// Owns isolated shared-obligation migration and fail-closed load regressions.
// Does not access live stores or certify remote settlement and provider effects.
#[cfg(test)]
mod session_obligation_storage_tests {
    use super::*;

    fn connection() -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        ensure_sqlite_state_schema(&connection).unwrap();
        connection
    }

    fn metadata(connection: &rusqlite::Connection) -> Value {
        metadata_units_to_value(&read_metadata_units_unchecked(connection).unwrap()).unwrap()
    }

    fn replace_metadata(connection: &rusqlite::Connection, value: &Value) {
        // Deliberately bypass validation without defaulting a missing safety
        // field: malformed fixtures must reach the real strict reader.
        connection
            .execute("DELETE FROM state_metadata_units", [])
            .unwrap();
        for ((field, key), (kind, encoded)) in metadata_value_to_units(value.clone()).unwrap() {
            connection.execute(
                "INSERT INTO state_metadata_units(field_name,item_key,kind,value_json) VALUES(?1,?2,?3,?4)",
                rusqlite::params![field,key,kind,encoded],
            ).unwrap();
        }
    }

    #[test]
    fn project_containment_storage_requires_explicit_supported_ledger_before_effects() {
        for load in [false, true] {
            for invalid in [
                None,
                Some(Value::Null),
                Some(json!({})),
                Some(json!({"schemaVersion": 1, "entries": []})),
                Some(json!({"schemaVersion": 2, "entries": [], "checkpointOperations": []})),
                Some(
                    json!({"schemaVersion": 1, "entries": [{"kind": "future_kind"}], "checkpointOperations": []}),
                ),
                Some(
                    json!({"schemaVersion": 1, "entries": [], "checkpointOperations": [], "future": true}),
                ),
            ] {
                let connection = connection();
                let mut value = metadata(&connection);
                if let Some(invalid) = invalid {
                    value["engramSessionObligations"] = invalid;
                } else {
                    value
                        .as_object_mut()
                        .unwrap()
                        .remove("engramSessionObligations");
                }
                replace_metadata(&connection, &value);
                let before = connection.total_changes();
                let result = if load {
                    ensure_sqlite_state_schema_for_load(&connection).map(|_| ())
                } else {
                    ensure_sqlite_state_schema(&connection)
                };
                assert!(
                    result.is_err(),
                    "unsupported ledger must refuse the entire reader"
                );
                assert_eq!(connection.total_changes(), before);
                assert_eq!(metadata(&connection), value);
            }
        }
    }

    #[test]
    fn project_containment_storage_refuses_unknown_identity_confirmations_without_a_local_row() {
        for grant in ["", ENGRAM_UNCERTAIN_GRANT_UNKNOWN] {
            for load in [false, true] {
                let connection = connection();
                let root = std::env::current_dir().unwrap();
                let image = EngramUnsettledControlImage {
                    operation: "unsettled_control_image".into(),
                    routing_token: (!grant.is_empty()).then(|| "old-token".into()),
                    active_grant_id: None,
                    uncertain_grant_id: (!grant.is_empty()).then(|| grant.into()),
                    begins_recorded: true,
                    bind_in_progress: false,
                    checkpoint_in_progress: false,
                    existing_checkpoint: None,
                    runtime_running: false,
                    parked_admission_retry: None,
                    parked_bind_retry: None,
                    queued_prompts: Vec::new(),
                    begin_requested: None,
                    dispatch_generation: 1,
                };
                let entry = EngramSessionObligation {
                    id: "invalid-confirmation".into(),
                    project_id: "removed-project".into(),
                    authority: EngramAuthorityStoreKey {
                        database_path: root.join("original.db"),
                        project_id: "original-project".into(),
                    },
                    authority_confirmed: true,
                    connection: EngramConnectionConfig {
                        binary_path: PathBuf::from("engram"),
                        project_file: root.join(".engram-project"),
                        home: root.clone(),
                        project_root: root,
                        actor_id: "actor".into(),
                        actor_context: None,
                        session_id: "removed-session".into(),
                    },
                    external_ref: "termal:session:removed-session".into(),
                    local_session_id: "removed-session".into(),
                    owner_generation: 1,
                    prompt_id: None,
                    routing_token: image.routing_token.clone().unwrap_or_default(),
                    grant_id: grant.into(),
                    kind: EngramSessionObligationKind::CheckpointContainment,
                    phase: EngramSessionObligationPhase::Confirmed,
                    basis: EngramSessionObligationBasis::ControlStateUnsettled,
                    reason_code: "control_settlement_unconfirmed".into(),
                    frozen_request: serde_json::to_value(image).unwrap(),
                    settlement_evidence: json!({"operation":"turn_checkpoint", "response":{
                        "decision":"checkpointed", "receipt":{"grant_id":grant,"cursor":1,"confirmed_cursor":1}}}),
                };
                let mut value = metadata(&connection);
                value["engramSessionObligations"] =
                    json!({"schemaVersion":1,"entries":[entry],"checkpointOperations":[]});
                replace_metadata(&connection, &value);
                let before = connection.total_changes();
                let result = if load {
                    ensure_sqlite_state_schema_for_load(&connection).map(|_| ())
                } else {
                    ensure_sqlite_state_schema(&connection)
                };
                assert!(
                    result.is_err(),
                    "unknown original identity is never a confirmed obligation"
                );
                assert_eq!(connection.total_changes(), before);
                assert_eq!(metadata(&connection), value);
            }
        }
    }

    fn predecessor(connection: &rusqlite::Connection, version: &str) -> Value {
        let mut value = metadata(connection);
        value
            .as_object_mut()
            .unwrap()
            .remove("engramSessionObligations");
        replace_metadata(connection, &value);
        if version == "2" {
            connection
                .execute_batch(
                    "DROP TABLE state_metadata_units;
                    DELETE FROM meta WHERE key='metadata_units_generation';",
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO app_state(key,value_json) VALUES(?1,?2)",
                    rusqlite::params![SQLITE_METADATA_KEY, value.to_string()],
                )
                .unwrap();
        } else if version == "2-metadata-units-1" {
            connection.execute("UPDATE state_metadata_units SET value_json='null' WHERE kind IN ('object','array')", []).unwrap();
        }
        connection
            .execute(
                "UPDATE meta SET value=?1 WHERE key='schema_version'",
                [version],
            )
            .unwrap();
        value
    }

    #[test]
    fn project_containment_storage_migrates_supported_floors_without_rewriting_authority() {
        for old_version in [
            "2",
            "2-metadata-units-1",
            "2-metadata-units-2",
            "2-metadata-units-2-handoff-1",
            SQLITE_OBSERVATION_SCHEMA_VERSION,
        ] {
            for load in [false, true] {
                let connection = connection();
                let old_metadata = predecessor(&connection, old_version);
                let retained = r#"{ "session":{}, "engramUncertainGrantId":"original-possibly-sent", "unknownRecovery":{"key":"original-key"} }"#;
                connection
                    .execute(
                        "INSERT INTO sessions(id, value_json) VALUES('original', ?1)",
                        [retained],
                    )
                    .unwrap();
                let loaded = if load {
                    ensure_sqlite_state_schema_for_load(&connection).unwrap()
                } else {
                    ensure_sqlite_state_schema(&connection).unwrap();
                    None
                };
                if let Some(loaded) = loaded {
                    assert!(loaded.engram_session_obligations.entries.is_empty());
                }
                assert_eq!(
                    required_state_meta_value(&connection, "schema_version").unwrap(),
                    SQLITE_SCHEMA_VERSION
                );
                let mut expected = old_metadata;
                expected["engramSessionObligations"] =
                    serde_json::to_value(EngramSessionObligationLedger::empty()).unwrap();
                assert_eq!(metadata(&connection), expected);
                let row: String = connection
                    .query_row(
                        "SELECT value_json FROM sessions WHERE id='original'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(
                    row, retained,
                    "normalized recovery authority remains byte exact"
                );
                let before = connection.total_changes();
                for old_floor in [SQLITE_OBSERVATION_PREDECESSOR, SQLITE_OBSERVATION_SCHEMA_VERSION] {
                    assert!(validate_sqlite_state_schema_for_version(&connection, old_floor).is_err());
                }
                assert_eq!(
                    connection.total_changes(),
                    before,
                    "the old handoff reader cannot write through the joint floor"
                );
                ensure_sqlite_state_schema_for_load(&connection).unwrap();
                assert_eq!(metadata(&connection), expected, "migration is idempotent");
            }
        }
    }

    #[test]
    fn project_containment_storage_rolls_back_the_entire_ordered_floor() {
        for old_version in [
            "2",
            "2-metadata-units-1",
            "2-metadata-units-2",
            "2-metadata-units-2-handoff-1",
            SQLITE_OBSERVATION_SCHEMA_VERSION,
        ] {
            let connection = connection();
            predecessor(&connection, old_version);
            let original = sqlite_state_user_table_names(&connection)
                .unwrap()
                .contains("state_metadata_units")
                .then(|| read_metadata_units_unchecked(&connection).unwrap());
            let provenance: Option<String> = connection
                .query_row(
                    "SELECT value_json FROM app_state WHERE key=?1",
                    [SQLITE_METADATA_KEY],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();
            let generation = metadata_unit_generation(&connection).unwrap();
            connection
                .execute_batch(&format!(
                    "CREATE TRIGGER refuse_joint_floor BEFORE UPDATE OF value ON meta
                WHEN OLD.key='schema_version' AND NEW.value='{SQLITE_SCHEMA_VERSION}'
                BEGIN SELECT RAISE(ABORT, 'injected joint floor failure'); END;"
                ))
                .unwrap();
            let error = ensure_sqlite_state_schema_for_load(&connection)
                .err()
                .expect("injected joint floor write must fail");
            assert!(format!("{error:#}").contains("injected joint floor failure"));
            assert_eq!(
                required_state_meta_value(&connection, "schema_version").unwrap(),
                old_version
            );
            assert_eq!(
                sqlite_state_user_table_names(&connection)
                    .unwrap()
                    .contains("state_metadata_units")
                    .then(|| read_metadata_units_unchecked(&connection).unwrap()),
                original,
                "ledger and intermediate normalization/handoff CAS roll back together"
            );
            assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
            let after: Option<String> = connection
                .query_row(
                    "SELECT value_json FROM app_state WHERE key=?1",
                    [SQLITE_METADATA_KEY],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();
            assert_eq!(
                after, provenance,
                "legacy provenance remains byte exact on rollback"
            );
            assert!(connection.is_autocommit());
        }
    }

    #[test]
    fn project_containment_storage_rejects_hidden_observation_keys_before_legacy_normalization() {
        for old_version in ["2", "2-metadata-units-1", SQLITE_METADATA_UNITS_VERSION,
            SQLITE_OBSERVATION_PREDECESSOR] {
            for key in ["engramRuntimeCheckpoint", "engramUnadmittedActiveTurn",
                "engramUnadmittedClosingJobs", "engramUnadmittedReports"] {
                for value in [Value::Null, json!([]), json!({})] {
                    for load in [false, true] {
                        let connection = connection();
                        predecessor(&connection, old_version);
                        let raw = json!({(key): value}).to_string();
                        connection.execute("INSERT INTO sessions(id,value_json) VALUES('hidden',?1)", [&raw]).unwrap();
                        let metadata = read_persisted_metadata_json(&connection).unwrap();
                        let tables = sqlite_state_user_table_names(&connection).unwrap();
                        let generation = metadata_unit_generation(&connection).unwrap();
                        let changes = connection.total_changes();
                        let result = if load {
                            ensure_sqlite_state_schema_for_load(&connection).map(|_| ())
                        } else { ensure_sqlite_state_schema(&connection) };
                        assert!(format!("{:#}", result.unwrap_err()).contains("observation custody"), "{old_version}/{key}");
                        assert_eq!(connection.total_changes(), changes);
                        assert_eq!(sqlite_state_user_table_names(&connection).unwrap(), tables);
                        assert_eq!(read_persisted_metadata_json(&connection).unwrap(), metadata);
                        assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
                        assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), old_version);
                        assert_eq!(connection.query_row("SELECT value_json FROM sessions WHERE id='hidden'", [],
                            |row| row.get::<_, String>(0)).unwrap(), raw);
                    }
                }
            }
        }
    }

    #[test]
    fn project_containment_storage_outbox_extension_rolls_back_ledger_generation_and_commit_failures() {
        for fault in ["ledger", "generation", "commit"] {
            let connection = connection();
            predecessor(&connection, SQLITE_OBSERVATION_SCHEMA_VERSION);
            let original = read_metadata_units_unchecked(&connection).unwrap();
            let generation = metadata_unit_generation(&connection).unwrap();
            match fault {
                "ledger" => connection.execute_batch("CREATE TEMP TRIGGER refuse_ledger BEFORE INSERT ON state_metadata_units
                    WHEN NEW.field_name='engramSessionObligations'
                    BEGIN SELECT RAISE(ABORT,'injected ledger failure'); END;").unwrap(),
                "generation" => connection.execute_batch("CREATE TEMP TRIGGER refuse_generation BEFORE UPDATE ON meta
                    WHEN OLD.key='metadata_units_generation'
                    BEGIN SELECT RAISE(ABORT,'injected generation failure'); END;").unwrap(),
                "commit" => connection.execute_batch("PRAGMA foreign_keys=ON;
                    CREATE TEMP TABLE commit_fault(id INTEGER PRIMARY KEY,
                        parent INTEGER REFERENCES commit_fault(id) DEFERRABLE INITIALLY DEFERRED);
                    CREATE TEMP TRIGGER refuse_commit AFTER INSERT ON state_metadata_units
                    WHEN NEW.field_name='engramSessionObligations' AND NEW.item_key=''
                    BEGIN INSERT INTO commit_fault(id,parent) VALUES(1,2); END;").unwrap(),
                _ => unreachable!(),
            }
            let error = ensure_sqlite_state_schema_for_load(&connection).err().expect("injected boundary must fail");
            let diagnostic = format!("{error:#}");
            if fault == "commit" {
                assert!(diagnostic.contains("failed to commit joint state reader-floor migration"), "{diagnostic}");
            } else { assert!(diagnostic.contains(&format!("injected {fault} failure")), "{diagnostic}"); }
            assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), original);
            assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
            assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), SQLITE_OBSERVATION_SCHEMA_VERSION);
            assert_eq!(connection.query_row("SELECT count(*) FROM messages", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
            if fault == "commit" {
                assert_eq!(connection.query_row("SELECT count(*) FROM commit_fault", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
            }
            assert!(connection.is_autocommit());
        }
    }

    #[test]
    fn project_containment_storage_preserves_raw_units_and_invalidates_old_captures() {
        let connection = connection();
        predecessor(&connection, SQLITE_OBSERVATION_SCHEMA_VERSION);
        connection.execute("UPDATE state_metadata_units SET value_json=' 7 ' WHERE field_name='federation' AND item_key='/version'", []).unwrap();
        connection.execute("UPDATE state_metadata_units SET value_json=' 1 ' WHERE field_name='nextMessageNumber' AND item_key=''", []).unwrap();
        connection
            .execute(
                "UPDATE meta SET value='7' WHERE key='metadata_units_generation'",
                [],
            )
            .unwrap();
        let original = read_metadata_units_unchecked(&connection).unwrap();
        let capture = PersistMetadataCapture::new(Arc::new(std::sync::atomic::AtomicU64::new(7)));
        ensure_sqlite_state_schema_for_load(&connection).unwrap();
        assert_eq!(metadata_unit_generation(&connection).unwrap(), 8);
        let mut upgraded = read_metadata_units_unchecked(&connection).unwrap();
        upgraded.retain(|(field, _), _| field != "engramSessionObligations");
        assert_eq!(
            upgraded, original,
            "unrelated unit bytes are not reserialized"
        );
        assert!(
            capture
                .verify(metadata_unit_generation(&connection).unwrap())
                .is_err()
        );
        let before = read_metadata_units_unchecked(&connection).unwrap();
        assert!(
            write_metadata_units(&connection, original, None, &[], Some(&capture), false).is_err()
        );
        assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), before);
    }

    #[test]
    fn project_containment_storage_preinitialization_snapshot_cannot_replace_seeded_authority() {
        let inner = StateInner::new();
        let stale = PersistedState::metadata_from_inner(&inner);
        let connection = connection();
        assert_eq!(metadata_unit_generation(&connection).unwrap(), 1);
        let before = read_metadata_units_unchecked(&connection).unwrap();
        assert!(
            write_metadata_units(
                &connection,
                project_metadata_units(&stale).unwrap(),
                None,
                &[],
                Some(&stale.metadata_capture),
                false
            )
            .is_err()
        );
        assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), before);
        assert_eq!(
            inner
                .metadata_predecessor
                .load(std::sync::atomic::Ordering::Acquire),
            0,
            "an old snapshot is never relabeled as initialized"
        );
    }

    #[test]
    fn project_containment_storage_live_source_recaptures_after_its_own_initialization() {
        let root = TestTempRoot::create("obligation-fresh-source");
        let inner = StateInner::new();
        let path = root.database_path();
        persist_state(&path, &inner).unwrap();
        assert_eq!(
            inner
                .metadata_predecessor
                .load(std::sync::atomic::Ordering::Acquire),
            2
        );
        let loaded = load_state(&path).unwrap().unwrap();
        assert!(loaded.engram_session_obligations.entries.is_empty());
    }

    #[test]
    fn project_containment_storage_refuses_mixed_predecessor_ledger_or_orphan_mirror() {
        for old_version in [SQLITE_OBSERVATION_PREDECESSOR, SQLITE_OBSERVATION_SCHEMA_VERSION] {
        for defect in ["ledger", "mirror"] {
            let connection = connection();
            predecessor(&connection, old_version);
            if defect == "ledger" {
                let mut units = read_metadata_units_unchecked(&connection).unwrap();
                units.extend(
                    metadata_value_to_units(
                        json!({"engramSessionObligations": EngramSessionObligationLedger::empty()}),
                    )
                    .unwrap(),
                );
                replace_metadata(&connection, &metadata_units_to_value(&units).unwrap());
            } else {
                connection.execute("INSERT INTO sessions(id,value_json) VALUES('orphan','{\"session\":{},\"engramCheckpointOperation\":{}}')", []).unwrap();
            }
            let original = read_metadata_units_unchecked(&connection).unwrap();
            let changes = connection.total_changes();
            assert!(
                ensure_sqlite_state_schema_for_load(&connection).is_err(),
                "{defect}"
            );
            assert_eq!(connection.total_changes(), changes);
            assert_eq!(
                read_metadata_units_unchecked(&connection).unwrap(),
                original
            );
            assert_eq!(
                required_state_meta_value(&connection, "schema_version").unwrap(),
                old_version
            );
        }
        }
    }

    #[test]
    fn project_containment_storage_generation_failure_or_exhaustion_keeps_the_predecessor() {
        for exhausted in [false, true] {
            let connection = connection();
            predecessor(&connection, SQLITE_OBSERVATION_SCHEMA_VERSION);
            if exhausted {
                connection
                    .execute(
                        "UPDATE meta SET value=?1 WHERE key='metadata_units_generation'",
                        [u64::MAX.to_string()],
                    )
                    .unwrap();
            } else {
                connection.execute_batch("CREATE TRIGGER refuse_obligation_generation BEFORE UPDATE OF value ON meta
                    WHEN old.key='metadata_units_generation' BEGIN SELECT RAISE(ABORT,'injected generation failure'); END;").unwrap();
            }
            let original = read_metadata_units_unchecked(&connection).unwrap();
            let generation = metadata_unit_generation(&connection).unwrap();
            assert!(ensure_sqlite_state_schema_for_load(&connection).is_err());
            assert_eq!(
                read_metadata_units_unchecked(&connection).unwrap(),
                original
            );
            assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
            assert_eq!(
                required_state_meta_value(&connection, "schema_version").unwrap(),
                SQLITE_OBSERVATION_SCHEMA_VERSION
            );
            assert!(connection.is_autocommit());
        }
    }

    #[test]
    fn project_containment_storage_rechecks_the_header_after_another_opener_finishes() {
        let root = TestTempRoot::create("obligation-concurrent-opener");
        let path = root.database_path();
        let connection = rusqlite::Connection::open(&path).unwrap();
        ensure_sqlite_state_schema(&connection).unwrap();
        predecessor(&connection, SQLITE_OBSERVATION_SCHEMA_VERSION);
        let generation = metadata_unit_generation(&connection).unwrap();
        let other = rusqlite::Connection::open(&path).unwrap();
        TEST_BEFORE_OBLIGATION_MIGRATION_BEGIN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                ensure_sqlite_state_schema_for_load(&other).unwrap();
            }))
        });
        ensure_sqlite_state_schema_for_load(&connection).unwrap();
        assert_eq!(
            metadata_unit_generation(&connection).unwrap(),
            generation + 1,
            "the opener which observed an old header does not migrate twice"
        );
        assert_eq!(
            required_state_meta_value(&connection, "schema_version").unwrap(),
            SQLITE_SCHEMA_VERSION
        );
        assert!(
            metadata(&connection)["engramSessionObligations"]["entries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
