// Split out of persist.rs: owns the existing current-schema and authority
// regressions, including validate-before-mutate and indexed paging checks.
// Does not own production schema logic or the separate message-layout,
// overview, permission-hardening and application persistence test suites.

#[cfg(test)]
mod sqlite_schema_tests {
    use super::*;

    fn expect_startup_schema_error(
        result: Result<Option<PersistedState>>,
        message: &str,
    ) -> anyhow::Error {
        match result {
            Ok(_) => panic!("{message}"),
            Err(error) => error,
        }
    }

    #[test]
    fn sqlite_schema_guard_rejects_noncurrent_versions_before_creating_state_tables() {
        for unsupported_version in ["1", "3"] {
            let connection =
                rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
            connection
                .execute_batch(&format!(
                    "
                    CREATE TABLE meta (
                      key TEXT PRIMARY KEY,
                      value TEXT NOT NULL
                    );
                    INSERT INTO meta(key, value)
                    VALUES('schema_version', '{unsupported_version}');
                    "
                ))
                .expect("seed unsupported schema version");

            let error = ensure_sqlite_state_schema(&connection)
                .expect_err("unsupported schema version should be rejected");

            let rendered = format!("{error:#}");
            assert!(
                rendered.contains("unsupported state database schema"),
                "{rendered}"
            );
            assert!(
                rendered.contains(&format!("found version `{unsupported_version}`")),
                "{rendered}"
            );
            assert!(
                rendered.contains("Move or delete `termal.sqlite`"),
                "{rendered}"
            );
            assert!(rendered.contains("not migrated"), "{rendered}");
            let state_table_count: u32 = connection
                .query_row(
                    "
                    SELECT COUNT(*)
                    FROM sqlite_master
                    WHERE type = 'table'
                      AND name IN ('app_state', 'sessions', 'delegations')
                    ",
                    [],
                    |row| row.get(0),
                )
                .expect("state table count should be queryable");
            assert_eq!(state_table_count, 0);
        }
    }

    #[test]
    fn sqlite_schema_guard_accepts_the_current_normalized_schema() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");

        ensure_sqlite_state_schema(&connection).expect("fresh current schema should initialize");
        ensure_sqlite_state_schema(&connection).expect("current schema should validate on reopen");

        assert_eq!(
            sqlite_state_user_table_names(&connection)
                .expect("current table inventory should remain readable"),
            BTreeSet::from([
                "app_state".to_owned(),
                "board_cards".to_owned(),
                "delegations".to_owned(),
                "messages".to_owned(),
                "meta".to_owned(),
                "response_board_tabs".to_owned(),
                "session_overviews".to_owned(),
                "session_prompt_histories".to_owned(),
                "sessions".to_owned(),
            ])
        );
        let stored_version: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("current schema version should exist");
        assert_eq!(stored_version, "2");
        let prompt_history_storage_version: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                rusqlite::params![SQLITE_PROMPT_HISTORY_STORAGE_KEY],
                |row| row.get(0),
            )
            .expect("current prompt-history authority marker should exist");
        assert_eq!(prompt_history_storage_version, "1");
        let default_board_tab_count: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM response_board_tabs WHERE id = ?1",
                rusqlite::params![RESPONSE_BOARD_DEFAULT_TAB_ID],
                |row| row.get(0),
            )
            .expect("default response-board tab should be queryable");
        assert_eq!(default_board_tab_count, 1);
        let board_index_count: u32 = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM sqlite_master
                 WHERE type = 'index'
                   AND name = 'board_cards_tab_placement_created_idx'",
                [],
                |row| row.get(0),
            )
            .expect("response-board index should be queryable");
        assert_eq!(board_index_count, 1);
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM app_state WHERE key = ?1",
                    rusqlite::params![SQLITE_METADATA_KEY],
                    |row| row.get::<_, u32>(0),
                )
                .expect("fresh metadata authority count should be queryable"),
            0,
            "schema initialization alone must remain a valid never-persisted database"
        );
        assert_eq!(
            first_normalized_state_authority_table_with_rows(&connection)
                .expect("fresh authority tables should be inspectable"),
            None
        );
    }

    #[test]
    fn sqlite_schema_guard_rejects_unversioned_existing_database_without_mutation() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        connection
            .execute_batch(
                "CREATE TABLE sessions (
                   id TEXT PRIMARY KEY,
                   value_json TEXT NOT NULL
                 );",
            )
            .expect("obsolete unversioned table should seed");

        let error = ensure_sqlite_state_schema(&connection)
            .expect_err("unversioned existing state should be rejected");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("unsupported state database schema"),
            "{rendered}"
        );
        assert!(rendered.contains("missing `meta` table"), "{rendered}");
        assert!(
            rendered.contains("Move or delete `termal.sqlite`"),
            "{rendered}"
        );
        assert_eq!(
            sqlite_state_user_table_names(&connection)
                .expect("table inventory should remain readable"),
            BTreeSet::from(["sessions".to_owned()]),
            "rejection must not initialize current tables into an obsolete database"
        );
    }

    #[test]
    fn sqlite_schema_guard_rejects_partial_same_version_schema_without_repairing_it() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_state_schema(&connection).expect("fresh current schema should initialize");
        connection
            .execute_batch("DROP TABLE delegations;")
            .expect("required table should drop for partial-schema fixture");

        let error = ensure_sqlite_state_schema(&connection)
            .expect_err("partial same-version schema should be rejected");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("missing required tables"), "{rendered}");
        assert!(rendered.contains("delegations"), "{rendered}");
        assert!(
            !sqlite_state_user_table_names(&connection)
                .expect("partial table inventory should remain readable")
                .contains("delegations"),
            "rejection must not recreate a missing current table"
        );
    }

    #[test]
    fn sqlite_schema_guard_rejects_missing_authority_rows_without_mutation() {
        for missing_key in ["schema_version", SQLITE_PROMPT_HISTORY_STORAGE_KEY] {
            let connection =
                rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
            ensure_sqlite_state_schema(&connection)
                .expect("fresh current schema should initialize");
            connection
                .execute(
                    "DELETE FROM meta WHERE key = ?1",
                    rusqlite::params![missing_key],
                )
                .expect("authority row should delete for fixture");
            let changes_before_rejection = connection.total_changes();

            let error = ensure_sqlite_state_schema(&connection)
                .expect_err("missing authority row should be rejected");
            let rendered = format!("{error:#}");
            assert!(rendered.contains(missing_key), "{rendered}");
            assert_eq!(connection.total_changes(), changes_before_rejection);
            assert_eq!(
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM meta WHERE key = ?1",
                        rusqlite::params![missing_key],
                        |row| row.get::<_, u32>(0),
                    )
                    .expect("authority row count should remain readable"),
                0,
                "rejection must not restore a missing authority row"
            );
        }
    }

    #[test]
    fn sqlite_schema_guard_rejects_legacy_embedded_app_state_arrays() {
        for embedded_key in ["sessions", "delegations"] {
            let connection =
                rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
            ensure_sqlite_state_schema(&connection)
                .expect("fresh current schema should initialize");
            let encoded = format!(r#"{{"{embedded_key}":[{{"id":"legacy"}}]}}"#);
            connection
                .execute(
                    "INSERT INTO app_state(key, value_json) VALUES(?1, ?2)",
                    rusqlite::params![SQLITE_METADATA_KEY, encoded],
                )
                .expect("embedded app-state fixture should insert");
            let changes_before_rejection = connection.total_changes();

            let error = expect_startup_schema_error(
                ensure_sqlite_state_schema_for_load(&connection),
                "embedded app-state records should be rejected",
            );
            let rendered = format!("{error:#}");
            assert!(rendered.contains(embedded_key), "{rendered}");
            assert_eq!(connection.total_changes(), changes_before_rejection);
        }
    }

    #[test]
    fn sqlite_schema_guard_gives_reset_guidance_for_malformed_app_state_metadata() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_state_schema(&connection).expect("fresh current schema should initialize");
        connection
            .execute(
                "INSERT INTO app_state(key, value_json) VALUES(?1, '{ not json')",
                rusqlite::params![SQLITE_METADATA_KEY],
            )
            .expect("malformed app-state fixture should insert");

        let error = expect_startup_schema_error(
            ensure_sqlite_state_schema_for_load(&connection),
            "malformed app-state metadata should be rejected",
        );
        let rendered = format!("{error:#}");
        assert!(rendered.contains("not valid JSON"), "{rendered}");
        assert!(
            rendered.contains("Move or delete `termal.sqlite`"),
            "{rendered}"
        );
    }

    #[test]
    fn sqlite_schema_guard_rejects_missing_metadata_with_normalized_rows_without_rewriting() {
        let state_root = TestTempRoot::create("termal-state-missing-metadata-guard");
        let path = state_root.database_path();
        {
            let connection = open_sqlite_state_connection_unconfigured(&path)
                .expect("file-backed sqlite should open");
            ensure_sqlite_state_schema_for_path(&connection, &path)
                .expect("fresh current schema should initialize");
            connection
                .execute(
                    "INSERT INTO sessions(id, value_json) VALUES('session-existing', '{}')",
                    [],
                )
                .expect("normalized session should seed");
            connection
                .execute(
                    "INSERT INTO messages(
                       session_id, position, message_id, value_json, overview_kind, is_user
                     ) VALUES('session-existing', 0, 'message-existing', '{}', 0, 1)",
                    [],
                )
                .expect("normalized message should seed");
            connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .expect("fixture WAL should checkpoint before byte comparison");
        }
        let bytes_before_rejection =
            fs::read(&path).expect("missing-metadata fixture bytes should be readable");

        let connection = open_sqlite_state_connection_unconfigured(&path)
            .expect("missing-metadata fixture should reopen");
        let error = expect_startup_schema_error(
            ensure_sqlite_state_schema_for_load_path(&connection, &path),
            "normalized rows without metadata must be rejected",
        );
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("missing app_state `metadataState`"),
            "{rendered}"
        );
        assert!(rendered.contains("sessions"), "{rendered}");
        drop(connection);
        assert_eq!(
            fs::read(&path).expect("rejected database bytes should remain readable"),
            bytes_before_rejection,
            "missing-metadata rejection must occur before persistent PRAGMAs or maintenance"
        );
    }

    #[test]
    fn sqlite_schema_guard_rejects_structurally_invalid_metadata_without_rewriting() {
        let state_root = TestTempRoot::create("termal-state-invalid-metadata-guard");
        let path = state_root.database_path();
        {
            let connection = open_sqlite_state_connection_unconfigured(&path)
                .expect("file-backed sqlite should open");
            ensure_sqlite_state_schema_for_path(&connection, &path)
                .expect("fresh current schema should initialize");
            connection
                .execute(
                    "INSERT INTO app_state(key, value_json) VALUES(?1, ?2)",
                    rusqlite::params![
                        SQLITE_METADATA_KEY,
                        r#"{"nextSessionNumber":1,"nextMessageNumber":1,"projects":"wrong","sessions":[]}"#
                    ],
                )
                .expect("structurally invalid metadata should seed");
            connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .expect("fixture WAL should checkpoint before byte comparison");
        }
        let bytes_before_rejection =
            fs::read(&path).expect("invalid-metadata fixture bytes should be readable");

        let connection = open_sqlite_state_connection_unconfigured(&path)
            .expect("invalid-metadata fixture should reopen");
        let error = expect_startup_schema_error(
            ensure_sqlite_state_schema_for_load_path(&connection, &path),
            "structurally invalid metadata must be rejected",
        );
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("persisted state metadata does not match the current metadata shape"),
            "{rendered}"
        );
        assert!(
            rendered.contains("\"wrong\""),
            "the rejection should retain the stable invalid-fixture marker: {rendered}"
        );
        assert!(
            !rendered.contains(" at line "),
            "raw-string deserialization must retain the previous location-free rejection: \
             {rendered}"
        );
        assert!(
            rendered.contains("Move or delete `termal.sqlite`"),
            "{rendered}"
        );
        drop(connection);
        assert_eq!(
            fs::read(&path).expect("rejected database bytes should remain readable"),
            bytes_before_rejection,
            "invalid-metadata rejection must occur before persistent PRAGMAs or maintenance"
        );
    }

    #[test]
    fn sqlite_schema_guard_accepts_normalized_rows_with_current_metadata() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_state_schema(&connection).expect("fresh current schema should initialize");
        seed_current_state_metadata(&connection);
        connection
            .execute(
                "INSERT INTO sessions(id, value_json) VALUES('session-existing', '{}')",
                [],
            )
            .expect("normalized session should seed");
        connection
            .execute(
                "INSERT INTO messages(
                   session_id, position, message_id, value_json, overview_kind, is_user
                 ) VALUES('session-existing', 0, 'message-existing', '{}', 0, 1)",
                [],
            )
            .expect("normalized message should seed");

        let persisted = ensure_sqlite_state_schema_for_load(&connection)
            .expect("current metadata and normalized rows should validate");
        assert!(
            persisted.is_some(),
            "current metadata should be returned to the loader"
        );
        for table_name in ["sessions", "messages"] {
            let count = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table_name}"), [], |row| {
                    row.get::<_, u32>(0)
                })
                .expect("normalized row count should remain readable");
            assert_eq!(
                count, 1,
                "schema validation must preserve `{table_name}` rows"
            );
        }
    }

    #[test]
    fn sqlite_write_only_schema_setup_skips_legacy_scan_and_typed_metadata_deserialization() {
        let state_root = TestTempRoot::create("termal-state-write-only-schema-setup");
        let path = state_root.database_path();
        {
            let connection = open_sqlite_state_connection_unconfigured(&path)
                .expect("file-backed sqlite should open");
            ensure_sqlite_state_schema_for_path(&connection, &path)
                .expect("fresh current schema should initialize");
            seed_current_state_metadata(&connection);
            let transaction = connection
                .unchecked_transaction()
                .expect("large session fixture transaction should begin");
            for index in 0..2_048 {
                transaction
                    .execute(
                        "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)",
                        rusqlite::params![format!("session-{index:04}"), r#"{"session":{}}"#],
                    )
                    .expect("current session fixture should insert");
            }
            transaction
                .execute(
                    "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)",
                    rusqlite::params![
                        "legacy-session",
                        r#"{"session":{"promptHistory":["legacy prompt"]}}"#
                    ],
                )
                .expect("legacy session fixture should insert");
            transaction
                .commit()
                .expect("large session fixture should commit");
        }

        let write_connection = open_sqlite_state_connection_unconfigured(&path)
            .expect("write-only connection should open");
        reset_legacy_embedded_session_scan_count();
        reset_persisted_state_metadata_deserialize_count();
        ensure_sqlite_state_schema_for_path(&write_connection, &path)
            .expect("write-only schema setup should not inspect session JSON");
        assert_eq!(
            legacy_embedded_session_scan_count(),
            0,
            "write-only setup must not execute the legacy session JSON scan"
        );
        assert_eq!(
            persisted_state_metadata_deserialize_count(),
            0,
            "write-only setup must not deserialize persisted metadata"
        );
        drop(write_connection);

        let load_connection = open_sqlite_state_connection_unconfigured(&path)
            .expect("startup connection should open");
        reset_legacy_embedded_session_scan_count();
        reset_persisted_state_metadata_deserialize_count();
        let error = expect_startup_schema_error(
            ensure_sqlite_state_schema_for_load_path(&load_connection, &path),
            "startup must still reject embedded prompt-history authority",
        );
        assert_eq!(
            legacy_embedded_session_scan_count(),
            1,
            "startup must execute the legacy session JSON scan exactly once"
        );
        assert_eq!(
            persisted_state_metadata_deserialize_count(),
            1,
            "startup must deserialize persisted metadata exactly once"
        );
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(
                "session `legacy-session` still contains embedded `session.promptHistory`"
            ),
            "{rendered}"
        );
    }

    #[test]
    fn sqlite_schema_guard_rejects_embedded_prompt_history_but_not_malformed_rows() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_state_schema(&connection).expect("fresh current schema should initialize");
        seed_current_state_metadata(&connection);
        for (label, prompt_history) in [("populated", r#"["remember me"]"#), ("empty", "[]")] {
            let encoded = format!(r#"{{"session":{{"promptHistory":{prompt_history}}}}}"#);
            connection
                .execute(
                    "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)",
                    rusqlite::params![label, encoded],
                )
                .expect("embedded prompt-history fixture should insert");

            let error = expect_startup_schema_error(
                ensure_sqlite_state_schema_for_load(&connection),
                "an embedded prompt-history array should be rejected",
            );
            let rendered = format!("{error:#}");
            assert!(rendered.contains("session.promptHistory"), "{rendered}");
            assert!(rendered.contains(label), "{rendered}");

            connection
                .execute("DELETE FROM sessions WHERE id = ?1", [label])
                .expect("embedded prompt-history fixture should delete");
        }
        for (label, prompt_history) in [
            ("null", "null"),
            ("string", r#""damaged""#),
            ("object", r#"{"damaged":true}"#),
        ] {
            let encoded = format!(r#"{{"session":{{"promptHistory":{prompt_history}}}}}"#);
            connection
                .execute(
                    "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)",
                    rusqlite::params![label, encoded],
                )
                .expect("non-array prompt-history fixture should insert");
        }
        ensure_sqlite_state_schema_for_load(&connection)
            .expect("non-array prompt-history values are row corruption, not legacy authority");
        connection
            .execute(
                "INSERT INTO sessions(id, value_json) VALUES('damaged', '{ not json')",
                [],
            )
            .expect("malformed quarantinable row should insert");
        ensure_sqlite_state_schema_for_load(&connection)
            .expect("malformed session JSON is not obsolete-schema evidence");
        connection
            .execute(
                "INSERT INTO sessions(id, value_json) VALUES('current', '{\"session\":{}}')",
                [],
            )
            .expect("current session fixture should insert");
        ensure_sqlite_state_schema_for_load(&connection)
            .expect("a session without the embedded prompt-history key should remain current");
    }

    #[test]
    fn sqlite_schema_guard_rejects_foreign_column_shape_without_rewriting_the_database() {
        let state_root = TestTempRoot::create("termal-state-column-guard");
        let path = state_root.database_path();
        {
            let connection = open_sqlite_state_connection_unconfigured(&path)
                .expect("file-backed sqlite should open");
            ensure_sqlite_state_schema_for_path(&connection, &path)
                .expect("fresh current schema should initialize");
            connection
                .execute("ALTER TABLE messages ADD COLUMN foreign_shape TEXT", [])
                .expect("foreign column fixture should add");
            connection
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .expect("fixture WAL should checkpoint before byte comparison");
        }
        let bytes_before_rejection =
            fs::read(&path).expect("foreign-column fixture bytes should be readable");

        let connection = open_sqlite_state_connection_unconfigured(&path)
            .expect("foreign-column fixture should reopen");
        let error = ensure_sqlite_state_schema_for_path(&connection, &path)
            .expect_err("foreign current-table columns should be rejected");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("unexpected columns"), "{rendered}");
        assert!(rendered.contains("foreign_shape"), "{rendered}");
        drop(connection);
        assert_eq!(
            fs::read(&path).expect("rejected database bytes should remain readable"),
            bytes_before_rejection,
            "column-shape rejection must occur before persistent PRAGMAs or maintenance"
        );
    }

    #[test]
    fn sqlite_schema_guard_accepts_extra_ignored_tables() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_state_schema(&connection).expect("fresh current schema should initialize");
        connection
            .execute_batch("CREATE TABLE mailboxes (id TEXT PRIMARY KEY);")
            .expect("ignored legacy table should seed");

        ensure_sqlite_state_schema(&connection)
            .expect("extra ignored tables must not invalidate current v2 state");
        assert!(
            sqlite_state_user_table_names(&connection)
                .expect("table inventory should remain readable")
                .contains("mailboxes")
        );
    }

    #[test]
    fn current_schema_backfills_compact_session_overview_metadata() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        seed_current_state_core_tables(&connection);
        connection
            .execute_batch(
                "ALTER TABLE messages DROP COLUMN overview_kind;
                 ALTER TABLE messages DROP COLUMN is_user;",
            )
            .expect("remove only the maintenance-owned columns");
        connection
            .execute_batch(
                "
                INSERT INTO sessions(id, value_json) VALUES('session-1', '{}');
                INSERT INTO messages(session_id, position, message_id, value_json)
                VALUES(
                  'session-1',
                  0,
                  'message-1',
                  '{\"type\":\"command\",\"id\":\"message-1\",\"author\":\"you\",\"status\":\"error\"}'
                ), (
                  'session-1',
                  1,
                  'message-2',
                  '{\"type\":\"text\",\"id\":\"message-2\",\"author\":\"you\",\"text\":\"Human prompt\"}'
                ), (
                  'session-1',
                  2,
                  'message-3',
                  '{\"type\":\"text\",\"id\":\"message-3\",\"author\":\"you\",\"text\":\"Peer prompt\",\"source\":{\"sessionId\":\"peer-session\",\"name\":\"Peer\"}}'
                );
                ",
            )
            .expect("current schema maintenance fixture should initialize");
        seed_current_state_auxiliary_tables(&connection);
        seed_current_state_metadata(&connection);

        ensure_sqlite_state_schema(&connection).expect("v2 overview metadata should backfill");
        assert_eq!(
            sqlite_state_table_columns(&connection, "messages")
                .expect("post-maintenance message columns should be readable"),
            BTreeSet::from([
                "is_user".to_owned(),
                "message_id".to_owned(),
                "overview_kind".to_owned(),
                "position".to_owned(),
                "session_id".to_owned(),
                "value_json".to_owned(),
            ]),
            "the two tolerated missing message columns must be restored before boot continues"
        );

        let value_blob: Vec<u8> = connection
            .query_row(
                "SELECT value_blob
                 FROM session_overviews
                 WHERE session_id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .expect("backfilled overview blob should exist");
        assert_eq!(
            value_blob,
            vec![
                encode_conversation_overview_message(ConversationOverviewKind::Error, true,),
                encode_conversation_overview_message(ConversationOverviewKind::Text, true,),
                encode_conversation_overview_message(ConversationOverviewKind::Text, true,),
            ]
        );
    }

    #[test]
    fn current_schema_rejects_singleton_response_board_without_rewriting() {
        let state_root = TestTempRoot::create("termal-state-singleton-response-board-guard");
        let path = state_root.database_path();
        {
            let connection = open_sqlite_state_connection_unconfigured(&path)
                .expect("file-backed sqlite should open");
            ensure_sqlite_state_schema_for_path(&connection, &path)
                .expect("fresh current schema should initialize");
            seed_current_state_metadata(&connection);
            connection
                .execute_batch(
                    "
                    DROP TABLE board_cards;
                    CREATE TABLE board_cards (
                      id TEXT PRIMARY KEY,
                      x REAL NOT NULL,
                      y REAL NOT NULL,
                      w REAL NOT NULL,
                      h REAL NOT NULL,
                      snapshot_json TEXT NOT NULL,
                      source_session_id TEXT NOT NULL,
                      source_message_id TEXT NOT NULL,
                      created_at TEXT NOT NULL
                    );
                    INSERT INTO board_cards(
                      id, x, y, w, h, snapshot_json,
                      source_session_id, source_message_id, created_at
                    ) VALUES(
                      'legacy-card', 1.0, 2.0, 3.0, 4.0, '{}',
                      'session-1', 'message-1', '2026-01-01T00:00:00Z'
                    );
                    PRAGMA wal_checkpoint(TRUNCATE);
                    ",
                )
                .expect("singleton response-board fixture should initialize");
        }
        let bytes_before_rejection =
            fs::read(&path).expect("singleton response-board fixture should be readable");

        let connection = open_sqlite_state_connection_unconfigured(&path)
            .expect("singleton response-board fixture should reopen");
        let error = ensure_sqlite_state_schema_for_path(&connection, &path)
            .expect_err("singleton response-board schema must be rejected");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("table `board_cards` has missing columns"),
            "{rendered}"
        );
        assert!(rendered.contains("has_canvas_position"), "{rendered}");
        assert!(rendered.contains("placement"), "{rendered}");
        assert!(rendered.contains("tab_id"), "{rendered}");
        assert!(rendered.contains("not migrated"), "{rendered}");
        assert!(
            rendered.contains("Move or delete `termal.sqlite`"),
            "{rendered}"
        );
        drop(connection);
        assert_eq!(
            fs::read(&path).expect("rejected response-board database should remain readable"),
            bytes_before_rejection,
            "singleton response-board rejection must precede persistent PRAGMAs or maintenance"
        );
    }

    #[test]
    fn current_schema_isolates_malformed_overview_backfill_per_session() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        seed_current_state_core_tables(&connection);
        connection
            .execute_batch(
                "ALTER TABLE messages DROP COLUMN overview_kind;
                 ALTER TABLE messages DROP COLUMN is_user;",
            )
            .expect("remove only the maintenance-owned columns");
        connection
            .execute_batch(
                "
                INSERT INTO sessions(id, value_json)
                VALUES('healthy-local', '{}'), ('gapped-local', '{}');
                INSERT INTO messages(session_id, position, message_id, value_json)
                VALUES(
                  'healthy-local',
                  0,
                  'healthy-message',
                  '{\"type\":\"text\",\"id\":\"healthy-message\",\"author\":\"agent\",\"text\":\"ok\"}'
                ), (
                  'gapped-local',
                  4,
                  'gapped-message',
                  '{\"type\":\"text\",\"id\":\"gapped-message\",\"author\":\"agent\",\"text\":\"bad\"}'
                );
                ",
            )
            .expect("mixed local fixture should initialize");
        seed_current_state_auxiliary_tables(&connection);
        seed_current_state_metadata(&connection);

        ensure_sqlite_state_schema(&connection)
            .expect("one malformed session must not abort global schema startup");

        let healthy_blob: Vec<u8> = connection
            .query_row(
                "SELECT value_blob
                 FROM session_overviews
                 WHERE session_id = 'healthy-local'",
                [],
                |row| row.get(0),
            )
            .expect("healthy local session should still be backfilled");
        assert_eq!(
            healthy_blob,
            vec![encode_conversation_overview_message(
                ConversationOverviewKind::Text,
                false,
            )]
        );
        let malformed_overview_count: u32 = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM session_overviews
                 WHERE session_id = 'gapped-local'",
                [],
                |row| row.get(0),
            )
            .expect("malformed overview count should be queryable");
        assert_eq!(malformed_overview_count, 0);
    }

    #[test]
    fn fresh_state_schema_does_not_create_coordination_tables() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_state_schema(&connection).expect("state schema should initialize");

        let coordination_table_count: u32 = connection
            .query_row(
                "
                SELECT COUNT(*)
                FROM sqlite_master
                WHERE type = 'table'
                  AND (
                    name LIKE 'mailbox%'
                    OR name LIKE 'coordination_board_%'
                  )
                ",
                [],
                |row| row.get(0),
            )
            .expect("coordination table count should be queryable");
        assert_eq!(coordination_table_count, 0);
    }

    #[test]
    fn transcript_schema_uses_indexed_range_and_cursor_queries() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_state_schema(&connection).expect("state schema should initialize");

        let range_plan: Vec<String> = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT position, value_json
                 FROM messages
                 WHERE session_id = ?1 AND position >= ?2 AND position < ?3
                 ORDER BY position ASC",
            )
            .expect("range query plan should prepare")
            .query_map(rusqlite::params!["session-1", 10_i64, 20_i64], |row| {
                row.get(3)
            })
            .expect("range query plan should execute")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("range query plan rows should decode");
        assert!(
            range_plan.iter().any(|detail| {
                detail.contains("SEARCH messages USING INDEX")
                    && detail.contains("session_id=? AND position>? AND position<?")
            }),
            "range query must search the (session_id, position) index: {range_plan:?}"
        );
        assert!(
            range_plan.iter().all(|detail| {
                !detail.contains("SCAN messages") && !detail.contains("USE TEMP B-TREE")
            }),
            "range query must use indexed ordering without a scan or sort: {range_plan:?}"
        );

        let cursor_plan: Vec<String> = connection
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT position
                 FROM messages
                 WHERE session_id = ?1 AND message_id = ?2",
            )
            .expect("cursor query plan should prepare")
            .query_map(rusqlite::params!["session-1", "message-1"], |row| {
                row.get(3)
            })
            .expect("cursor query plan should execute")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("cursor query plan rows should decode");
        assert!(
            cursor_plan.iter().any(|detail| {
                detail.contains("SEARCH messages")
                    && detail.contains("session_id=?")
                    && detail.contains("message_id=?")
            }),
            "cursor query must use the unique (session_id, message_id) index: {cursor_plan:?}"
        );
        assert!(
            cursor_plan
                .iter()
                .all(|detail| !detail.contains("SCAN messages")),
            "cursor query must not scan messages: {cursor_plan:?}"
        );
    }
}
