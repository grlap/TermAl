// SQLite maintenance-boundary tests split from src/persist.rs, alongside its
// sqlite_schema_tests module.
// Owns DDL/inventory agreement, validate-before-mutate and file-permission
// phase contracts. Does not own schema migrations, live databases, or HTTP tests.

mod sqlite_maintenance_tests {
    use super::*;

    #[test]
    fn response_board_ddl_inventory_and_runtime_setup_agree() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(SQLITE_STATE_CORE_SCHEMA_SQL)
            .unwrap();
        connection
            .execute_batch(SQLITE_STATE_AUXILIARY_SCHEMA_SQL)
            .unwrap();
        let expected_tables = CURRENT_SQLITE_STATE_TABLE_COLUMNS
            .iter()
            .map(|(table, _)| (*table).to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            sqlite_state_user_table_names(&connection).unwrap(),
            expected_tables
        );
        validate_sqlite_state_table_columns(&connection, CURRENT_SQLITE_STATE_TABLE_COLUMNS, false)
            .expect("raw DDL and guard inventory must agree before maintenance can mask drift");

        for _ in 0..2 {
            ensure_sqlite_response_board_schema(&connection).unwrap();
        }
        assert_eq!(
            sqlite_state_user_table_names(&connection).unwrap(),
            expected_tables
        );
        validate_sqlite_state_table_columns(&connection, CURRENT_SQLITE_STATE_TABLE_COLUMNS, false)
            .unwrap();
        let default_tab: (String, String, String, Option<String>, i64) = connection
            .query_row(
                "SELECT id, name, kind, project_id, sort_order FROM response_board_tabs",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            default_tab,
            (
                RESPONSE_BOARD_DEFAULT_TAB_ID.to_owned(),
                RESPONSE_BOARD_DEFAULT_TAB_NAME.to_owned(),
                "custom".to_owned(),
                None,
                0
            )
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM response_board_tabs", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );

        connection.execute(
            "INSERT INTO board_cards(id, x, y, w, h, snapshot_json, source_session_id, source_message_id, created_at)
             VALUES('card', 0, 0, 360, 420, '{}', 'session', 'message', 'now')", [],
        ).unwrap();
        let card_defaults: (String, String, bool) = connection
            .query_row(
                "SELECT tab_id, placement, has_canvas_position FROM board_cards WHERE id = 'card'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            card_defaults,
            (
                RESPONSE_BOARD_DEFAULT_TAB_ID.to_owned(),
                ResponseBoardCardPlacement::Placed.as_db_str().to_owned(),
                true
            )
        );
        let mut statement = connection
            .prepare("PRAGMA index_info(board_cards_tab_placement_created_idx)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(2))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(columns, ["tab_id", "placement", "created_at", "id"]);
        connection
            .prepare(&format!(
                "SELECT {} FROM board_cards",
                response_board_select_columns()
            ))
            .unwrap();
        connection.prepare(response_board_tab_select_sql()).unwrap();
    }

    fn prepare_schema(
        connection: &rusqlite::Connection,
        path: &FsPath,
        startup: bool,
    ) -> Result<()> {
        if startup {
            ensure_sqlite_state_schema_for_load_path(connection, path).map(|_| ())
        } else {
            ensure_sqlite_state_schema_for_path(connection, path)
        }
    }

    #[test]
    fn path_aware_setup_enforces_foreign_keys_on_fresh_and_reopened_connections() {
        for startup in [false, true] {
            let root = TestTempRoot::create("termal-schema-foreign-keys");
            let path = root.database_path();
            for _ in 0..2 {
                let connection = open_sqlite_state_connection_unconfigured(&path).unwrap();
                // The unconfigured handle makes no foreign-key promise. Pin
                // the setup obligation without depending on SQLite defaults.
                connection
                    .execute_batch("PRAGMA foreign_keys = OFF;")
                    .unwrap();
                prepare_schema(&connection, &path, startup).unwrap();
                assert_eq!(
                    connection
                        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i32>(0))
                        .unwrap(),
                    1
                );
                connection.execute_batch(
                    "INSERT INTO sessions(id, value_json) VALUES('parent', '{}');
                     INSERT INTO messages(session_id, position, message_id, value_json) VALUES('parent', 0, 'message', '{}');
                     INSERT INTO session_prompt_histories(session_id, value_json) VALUES('parent', '[]');
                     INSERT INTO session_overviews(session_id, value_blob) VALUES('parent', X'00');
                     DELETE FROM sessions WHERE id = 'parent';"
                ).unwrap();
                for table in ["messages", "session_prompt_histories", "session_overviews"] {
                    assert_eq!(
                        connection
                            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                                .get::<_, i64>(0))
                            .unwrap(),
                        0,
                        "{table} must cascade"
                    );
                }
            }
        }
    }

    #[test]
    fn non_array_embedded_authority_rejects_before_persistent_pragmas_or_maintenance() {
        for key in ["sessions", "delegations"] {
            for value in [json!({}), json!("wrong"), json!(7), json!(false)] {
                let root = TestTempRoot::create("termal-invalid-embedded-authority");
                let path = root.database_path();
                {
                    let connection = rusqlite::Connection::open(&path).unwrap();
                    seed_current_state_core_tables(&connection);
                    seed_current_state_auxiliary_tables(&connection);
                    let mut metadata = json!({"nextSessionNumber":1,"nextMessageNumber":1,"projects":[],"sessions":[]});
                    metadata[key] = value;
                    connection
                        .execute(
                            "INSERT INTO app_state VALUES(?1, ?2)",
                            rusqlite::params![SQLITE_METADATA_KEY, metadata.to_string()],
                        )
                        .unwrap();
                }
                let bytes_before = fs::read(&path).unwrap();
                let connection = open_sqlite_state_connection_unconfigured(&path).unwrap();
                let error = prepare_schema(&connection, &path, true).unwrap_err();
                assert_eq!(
                    format!("{error:#}"),
                    format!(
                        "failed to validate or initialize state database `{}`: {}",
                        path.display(),
                        reject_unsupported_state_schema(format!(
                            "app_state contains embedded `{key}` records or an invalid `{key}` shape"
                        ))
                    )
                );
                assert_eq!(
                    connection
                        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
                        .unwrap(),
                    "delete"
                );
                assert_eq!(
                    connection
                        .query_row("SELECT COUNT(*) FROM response_board_tabs", [], |row| row
                            .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
                drop(connection);
                assert!(
                    fs::read(&path).unwrap() == bytes_before,
                    "rejected metadata must not rewrite database bytes"
                );
            }
        }
    }

    #[test]
    fn setup_error_precedence_is_independent_of_permission_platform() {
        let path = FsPath::new("fixture.sqlite");
        assert_eq!(resolve_sqlite_state_setup_result(path, Ok(42), Ok(())).unwrap(), 42);
        let hardening = anyhow!("hardening failure").context("permission context");
        let expected = format!("{hardening:#}");
        assert_eq!(
            format!("{:#}", resolve_sqlite_state_setup_result(path, Ok(()), Err(hardening)).unwrap_err()),
            expected
        );
        for fail_hardening in [false, true] {
            let original = anyhow!("original maintenance failure").context("original setup context");
            let expected = format!("{original:#}");
            let error = resolve_sqlite_state_setup_result::<()>(
                path,
                Err(original),
                if fail_hardening { Err(anyhow!("secondary hardening failure")) } else { Ok(()) },
            ).unwrap_err();
            assert_eq!(format!("{error:#}"), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn both_permission_phases_cover_success_and_maintenance_failure() {
        use std::os::unix::fs::PermissionsExt;
        for startup in [false, true] {
            for fail_maintenance in [false, true] {
                let root = TestTempRoot::create("termal-schema-permission-phases");
                let path = root.database_path();
                {
                    let connection = rusqlite::Connection::open(&path).unwrap();
                    seed_current_state_core_tables(&connection);
                    seed_current_state_auxiliary_tables(&connection);
                    seed_current_state_metadata(&connection);
                    if fail_maintenance {
                        connection.execute_batch("CREATE TRIGGER fail_default_tab BEFORE INSERT ON response_board_tabs BEGIN SELECT RAISE(ABORT, 'forced board maintenance failure'); END;").unwrap();
                    }
                }
                fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
                let connection = open_sqlite_state_connection_unconfigured(&path).unwrap();
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600,
                    "main file must be hardened before WAL activation"
                );
                let wal = sqlite_sidecar_path(&path, "-wal");
                let shm = sqlite_sidecar_path(&path, "-shm");
                assert!(!wal.exists() && !shm.exists());
                configure_sqlite_state_connection(&connection).unwrap();
                connection
                    .execute("UPDATE meta SET value = value", [])
                    .unwrap();
                // Exercise real WAL/SHM files created after the opener's pass.
                // Loosen them so omitting the second pass makes this test fail,
                // even on hosts where SQLite inherits 0600 from the main file.
                for file in [&wal, &shm] {
                    assert!(file.is_file());
                    fs::set_permissions(file, fs::Permissions::from_mode(0o666)).unwrap();
                }
                let result = prepare_schema(&connection, &path, startup);
                if fail_maintenance {
                    assert!(
                        format!("{:#}", result.unwrap_err())
                            .contains("forced board maintenance failure")
                    );
                } else {
                    result.unwrap();
                }
                for file in [&path, &wal, &shm] {
                    assert_eq!(
                        fs::metadata(file).unwrap().permissions().mode() & 0o777,
                        0o600,
                        "{} must be hardened even on error",
                        file.display()
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn post_setup_hardening_failure_is_fatal_without_replacing_an_existing_setup_error() {
        let root = TestTempRoot::create("termal-post-setup-permission-failure");
        let path = root.database_path();
        let target = root.path().join("sidecar-target");
        fs::write(&path, []).unwrap();
        fs::write(&target, []).unwrap();
        std::os::unix::fs::symlink(&target, sqlite_sidecar_path(&path, "-wal")).unwrap();
        let error = finish_sqlite_state_file_setup(&path, Ok(())).unwrap_err();
        assert!(format!("{error:#}").contains("symlink"));
        let original = anyhow!("original maintenance failure").context("original setup context");
        let expected = format!("{original:#}");
        let error = finish_sqlite_state_file_setup::<()>(&path, Err(original)).unwrap_err();
        assert_eq!(format!("{error:#}"), expected);
    }
}
