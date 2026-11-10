// Owns disposable MAIN/coordination reader-floor migration and rollback witnesses.
// Does not activate permission, call peers or schedule work. New storage tests.
use super::*;

#[test]
fn direct_prompt_delivery_floor_keeps_unreadable_session_rows_quarantined() {
    for activated in [false, true] {
        for for_load in [false, true] {
            for payload in ["x'7b7d'", "CAST(x'80' AS TEXT)", "'{not json'"] {
                let connection = predecessor(activated);
                let old = if activated {
                    SQLITE_DIRECT_PROMPT_DELIVERY_IMPORT_PREDECESSOR
                } else {
                    SQLITE_DIRECT_PROMPT_DELIVERY_PREDECESSOR
                };
                connection
                    .execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [old])
                    .unwrap();
                connection
                    .execute(
                        &format!("INSERT INTO sessions VALUES('unreadable',{payload})"),
                        [],
                    )
                    .unwrap();
                let raw = || {
                    connection.query_row(
                    "SELECT typeof(value_json),CAST(value_json AS BLOB) FROM sessions WHERE id='unreadable'",
                    [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))).unwrap()
                };
                let before = raw();
                let path = FsPath::new("fixture.sqlite");
                let (records, _, skipped) =
                    load_session_records_from_sqlite_with_skipped(&connection, path).unwrap();
                assert!(records.is_empty());
                assert_eq!(skipped, 1);
                prepare_existing_sqlite_state_reader_floor(&connection, for_load).unwrap();
                assert_eq!(raw(), before);
                let (records, _, skipped) =
                    load_session_records_from_sqlite_with_skipped(&connection, path).unwrap();
                assert!(records.is_empty());
                assert_eq!(skipped, 1);
                assert_eq!(
                    required_state_meta_value(&connection, "schema_version").unwrap(),
                    if activated {
                        SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
                    } else {
                        SQLITE_SCHEMA_VERSION
                    }
                );
            }
        }
    }
}

#[test]
fn direct_prompt_delivery_floor_preserves_both_predecessors_and_existing_policy() {
    for activated in [false, true] {
        for for_load in [false, true] {
            let connection = predecessor(activated);
            let old = if activated {
                SQLITE_DIRECT_PROMPT_DELIVERY_IMPORT_PREDECESSOR
            } else {
                SQLITE_DIRECT_PROMPT_DELIVERY_PREDECESSOR
            };
            connection
                .execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [old])
                .unwrap();
            connection.execute("INSERT INTO state_metadata_units VALUES('federation','/promptPermissions','entry',?1)",
                [serde_json::to_string(&FederationPromptPermissions::default()).unwrap()]).unwrap();
            connection
                .execute(
                    "UPDATE state_metadata_units SET value_json=CAST(value_json AS INTEGER)+1
                 WHERE field_name='federation' AND item_key=''",
                    [],
                )
                .unwrap();
            read_metadata_units(&connection)
                .expect("the predecessor fixture is valid before migration");
            let previous = read_metadata_units_unchecked(&connection).unwrap();
            let generation = metadata_unit_generation(&connection).unwrap();
            let loaded = prepare_existing_sqlite_state_reader_floor(&connection, for_load).unwrap();
            if for_load {
                assert!(loaded.unwrap().federation.prompt_permissions.is_some());
            }
            assert_eq!(
                read_metadata_units_unchecked(&connection).unwrap(),
                previous
            );
            assert_eq!(
                metadata_unit_generation(&connection).unwrap(),
                generation + 1
            );
            assert_eq!(
                required_state_meta_value(&connection, "schema_version").unwrap(),
                if activated {
                    SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
                } else {
                    SQLITE_SCHEMA_VERSION
                }
            );
            assert_eq!(
                matches!(
                    read_engram_import_reconciliation_state(&connection).unwrap(),
                    EngramImportReconciliationState::Activated(_)
                ),
                activated
            );
            let settled = image(&connection);
            assert!(validate_sqlite_state_schema_for_version(&connection, old).is_err());
            prepare_existing_sqlite_state_reader_floor(&connection, for_load).unwrap();
            assert_eq!(image(&connection), settled);
        }
    }
}

#[test]
fn direct_prompt_delivery_keys_refuse_predecessors_without_writes() {
    for version in OLD_FLOORS.iter().copied().chain([
        SQLITE_DIRECT_PROMPT_PREDECESSOR,
        SQLITE_DIRECT_PROMPT_IMPORT_PREDECESSOR,
        SQLITE_DIRECT_PROMPT_DELIVERY_PREDECESSOR,
        SQLITE_DIRECT_PROMPT_DELIVERY_IMPORT_PREDECESSOR,
    ]) {
        for for_load in [false, true] {
            for key in ["remotePrompt", "remote_prompt"] {
                for value in [Value::Null, json!({}), json!({"schemaVersion":999})] {
                    let connection = match version {
                        SQLITE_DIRECT_PROMPT_PREDECESSOR
                        | SQLITE_DIRECT_PROMPT_DELIVERY_PREDECESSOR => predecessor(false),
                        SQLITE_DIRECT_PROMPT_IMPORT_PREDECESSOR
                        | SQLITE_DIRECT_PROMPT_DELIVERY_IMPORT_PREDECESSOR => predecessor(true),
                        _ => source(version, false),
                    };
                    connection
                        .execute(
                            "UPDATE meta SET value=?1 WHERE key='schema_version'",
                            [version],
                        )
                        .unwrap();
                    let mut row = json!({"promptDeliveries":{"retained":{}}});
                    row["promptDeliveries"]["retained"][key] = value;
                    connection
                        .execute(
                            "INSERT INTO sessions VALUES('historical',?1)",
                            [row.to_string()],
                        )
                        .unwrap();
                    let before = image(&connection);
                    let changes = connection.total_changes();
                    let error = prepare_existing_sqlite_state_reader_floor(&connection, for_load)
                        .err()
                        .unwrap();
                    assert!(format!("{error:#}").contains("direct-prompt delivery association"));
                    assert_eq!(connection.total_changes(), changes);
                    assert_eq!(image(&connection), before);
                }
            }
        }
    }
}

#[test]
fn direct_prompt_delivery_floor_failure_rolls_back_both_predecessors() {
    for activated in [false, true] {
        for phase in ["validated", "header", "readback"] {
            let connection = predecessor(activated);
            connection
                .execute(
                    "UPDATE meta SET value=?1 WHERE key='schema_version'",
                    [if activated {
                        SQLITE_DIRECT_PROMPT_DELIVERY_IMPORT_PREDECESSOR
                    } else {
                        SQLITE_DIRECT_PROMPT_DELIVERY_PREDECESSOR
                    }],
                )
                .unwrap();
            let before = image(&connection);
            TEST_DIRECT_PROMPT_FLOOR_FAULT.with(|fault| fault.set(Some(phase)));
            let result = prepare_existing_sqlite_state_reader_floor(&connection, true);
            TEST_DIRECT_PROMPT_FLOOR_FAULT.with(|fault| fault.set(None));
            assert!(result.is_err());
            assert_eq!(image(&connection), before);
            prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap();
        }
    }
}

fn predecessor(activated: bool) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    ensure_sqlite_state_schema(&connection).unwrap();
    if activated {
        let mut journal = EngramImportReconciliationJournal::activated_empty();
        journal.record(import_record()).unwrap();
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
    connection
        .execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version'",
            [if activated {
                SQLITE_DIRECT_PROMPT_IMPORT_PREDECESSOR
            } else {
                SQLITE_DIRECT_PROMPT_PREDECESSOR
            }],
        )
        .unwrap();
    connection
}

#[test]
fn direct_prompt_floor_preserves_raw_metadata_and_activated_import_without_grants() {
    for activated in [false, true] {
        let connection = predecessor(activated);
        let previous = read_metadata_units_unchecked(&connection).unwrap();
        let generation = metadata_unit_generation(&connection).unwrap();
        let loaded = prepare_existing_sqlite_state_reader_floor(&connection, true)
            .unwrap()
            .unwrap();
        assert!(loaded.federation.prompt_permissions.is_none());
        assert_eq!(
            read_metadata_units_unchecked(&connection).unwrap(),
            previous
        );
        assert_eq!(
            metadata_unit_generation(&connection).unwrap(),
            generation + 1
        );
        assert_eq!(
            required_state_meta_value(&connection, "schema_version").unwrap(),
            if activated {
                SQLITE_IMPORT_RECONCILIATION_SCHEMA_VERSION
            } else {
                SQLITE_SCHEMA_VERSION
            }
        );
        assert_eq!(
            matches!(
                read_engram_import_reconciliation_state(&connection).unwrap(),
                EngramImportReconciliationState::Activated(_)
            ),
            activated
        );
        let settled = image(&connection);
        prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap();
        assert_eq!(image(&connection), settled);
        assert!(
            validate_sqlite_state_schema_for_version(
                &connection,
                if activated {
                    SQLITE_DIRECT_PROMPT_IMPORT_PREDECESSOR
                } else {
                    SQLITE_DIRECT_PROMPT_PREDECESSOR
                }
            )
            .is_err()
        );
        assert_eq!(
            image(&connection),
            settled,
            "frozen predecessor refusal is read-only"
        );
    }
}

#[test]
fn direct_prompt_malformed_predecessor_units_preserve_reset_guidance_without_writes() {
    for activated in [false, true] {
        for for_load in [false, true] {
            for (field, key, kind) in [
                ("federation", "", "object"),
                ("projects", "", "object"),
                ("remoteTestRunFacts", "", "object"),
                ("federation", "/peers", "entry"),
            ] {
                let connection = predecessor(activated);
                connection
                    .execute(
                        "INSERT INTO state_metadata_units(field_name,item_key,kind,value_json)
                     VALUES(?1,?2,?3,'{broken json') ON CONFLICT(field_name,item_key)
                     DO UPDATE SET kind=excluded.kind,value_json=excluded.value_json",
                        rusqlite::params![field, key, kind],
                    )
                    .unwrap();
                let before = image(&connection);
                let changes = connection.total_changes();
                let error = prepare_existing_sqlite_state_reader_floor(&connection, for_load)
                    .err()
                    .expect("malformed metadata must refuse admission");
                let message = format!("{error:#}");
                assert!(
                    message.contains("metadata units are invalid"),
                    "{activated}/{for_load}/{field}/{key}: {message}"
                );
                assert!(
                    message.contains("Move or delete `termal.sqlite`"),
                    "{message}"
                );
                assert_eq!(connection.total_changes(), changes);
                assert_eq!(
                    image(&connection),
                    before,
                    "raw rows, header and generation retained"
                );
                assert!(connection.is_autocommit());
            }
        }
    }
}

#[test]
fn direct_prompt_floor_all_failure_cuts_roll_back_header_generation_and_data() {
    for activated in [false, true] {
        for phase in ["validated", "header", "readback"] {
            let connection = predecessor(activated);
            let before = image(&connection);
            TEST_DIRECT_PROMPT_FLOOR_FAULT.with(|fault| fault.set(Some(phase)));
            let result = prepare_existing_sqlite_state_reader_floor(&connection, true);
            TEST_DIRECT_PROMPT_FLOOR_FAULT.with(|fault| fault.set(None));
            let error = result
                .err()
                .expect("injected migration failure must refuse admission");
            assert!(format!("{error:#}").contains(phase), "{error:#}");
            assert_eq!(image(&connection), before, "{activated} {phase}");
            prepare_existing_sqlite_state_reader_floor(&connection, true).unwrap();
        }
    }
}

#[test]
fn direct_prompt_policy_key_presence_refuses_every_predecessor_before_normalization() {
    for version in OLD_FLOORS.iter().copied().chain([
        SQLITE_DIRECT_PROMPT_PREDECESSOR,
        SQLITE_DIRECT_PROMPT_IMPORT_PREDECESSOR,
    ]) {
        for key in ["promptPermissions", "prompt_permissions"] {
            for value in [
                Value::Null,
                json!({}),
                json!({"schemaVersion":999,"records":[]}),
            ] {
                let connection = if version == SQLITE_DIRECT_PROMPT_PREDECESSOR {
                    predecessor(false)
                } else if version == SQLITE_DIRECT_PROMPT_IMPORT_PREDECESSOR {
                    predecessor(true)
                } else {
                    source(version, false)
                };
                if version == "2" {
                    let encoded: String = connection
                        .query_row(
                            "SELECT value_json FROM app_state WHERE key=?1",
                            [SQLITE_METADATA_KEY],
                            |row| row.get(0),
                        )
                        .unwrap();
                    let mut metadata: Value = serde_json::from_str(&encoded).unwrap();
                    metadata["federation"][key] = value;
                    connection
                        .execute(
                            "UPDATE app_state SET value_json=?1 WHERE key=?2",
                            rusqlite::params![metadata.to_string(), SQLITE_METADATA_KEY],
                        )
                        .unwrap();
                } else {
                    connection
                        .execute(
                            "INSERT INTO state_metadata_units VALUES('federation',?1,'entry',?2)",
                            rusqlite::params![format!("/{key}"), value.to_string()],
                        )
                        .unwrap();
                }
                let before = image(&connection);
                let error = prepare_existing_sqlite_state_reader_floor(&connection, true)
                    .err()
                    .expect("predecessor policy data must be refused before normalization");
                assert!(
                    format!("{error:#}").contains("direct-prompt policy"),
                    "{version}: {error:#}"
                );
                assert_eq!(image(&connection), before);
            }
        }
    }
}

fn coordination_predecessor() -> rusqlite::Connection {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    connection
        .execute_batch(
            "DROP TABLE remote_prompt_intents; DROP TABLE remote_prompt_receives;
        INSERT INTO mailboxes VALUES('retained-box','source\ntarget','old',2);
        INSERT INTO mailbox_participants VALUES('retained-box','source','Source',0,'old',NULL);
        INSERT INTO mailbox_participants VALUES('retained-box','target','Target',0,'old',NULL);",
        )
        .unwrap();
    connection
        .execute(
            "UPDATE meta SET value=?1 WHERE key='coordination_schema_version'",
            [COORDINATION_DIRECT_PROMPT_PREDECESSOR],
        )
        .unwrap();
    validate_coordination_schema_version(&connection, COORDINATION_DIRECT_PROMPT_PREDECESSOR)
        .unwrap();
    connection
}

#[test]
fn direct_prompt_coordination_upgrade_preserves_old_rows_and_adds_no_work() {
    let connection = coordination_predecessor();
    let before = image(&connection);
    ensure_sqlite_coordination_schema(&connection).unwrap();
    let after = image(&connection);
    for (table, definition, rows) in &before {
        if table == "meta" {
            continue;
        }
        assert_eq!(
            after.iter().find(|(name, _, _)| name == table),
            Some(&(table.clone(), definition.clone(), rows.clone())),
            "{table}"
        );
    }
    for table in ["remote_prompt_intents", "remote_prompt_receives"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
    assert!(
        validate_coordination_schema_version(&connection, COORDINATION_DIRECT_PROMPT_PREDECESSOR)
            .is_err()
    );
    let settled = image(&connection);
    connection.execute_batch("PRAGMA query_only=ON;").unwrap();
    ensure_sqlite_coordination_schema(&connection).unwrap();
    assert_eq!(image(&connection), settled);
}

#[test]
fn direct_prompt_coordination_upgrade_failure_rolls_back_inventory_and_marker() {
    let connection = coordination_predecessor();
    let before = image(&connection);
    TEST_COORDINATION_UPGRADE_BEFORE_COMMIT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(|tx| {
            validate_current_coordination_schema(tx)?;
            bail!("injected direct-prompt coordination commit refusal")
        }))
    });
    let error = ensure_sqlite_coordination_schema(&connection).unwrap_err();
    assert!(format!("{error:#}").contains("commit refusal"));
    assert_eq!(image(&connection), before);
    ensure_sqlite_coordination_schema(&connection).unwrap();
    validate_current_coordination_schema(&connection).unwrap();
}

#[test]
fn direct_prompt_tables_under_predecessor_marker_are_refused_without_adoption() {
    let connection = coordination_predecessor();
    connection
        .execute_batch(&remote_prompt_storage_schema_sql())
        .unwrap();
    let before = image(&connection);
    assert!(ensure_sqlite_coordination_schema(&connection).is_err());
    assert_eq!(image(&connection), before);
}
