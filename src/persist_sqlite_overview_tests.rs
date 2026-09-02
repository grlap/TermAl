// Corruption and remote-identity regressions for `persist_sqlite_overview.rs`.

mod sqlite_overview_tests {
    use super::*;

    #[test]
    fn current_schema_skips_unreadable_and_invalid_proxy_overview_candidates() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        connection
            .execute_batch(
                "
                PRAGMA foreign_keys = ON;
                CREATE TABLE meta (
                  key TEXT PRIMARY KEY,
                  value TEXT NOT NULL
                );
                INSERT INTO meta(key, value) VALUES('schema_version', '2');
                INSERT INTO meta(key, value)
                VALUES('prompt_history_storage_version', '1');
                CREATE TABLE sessions (
                  id TEXT PRIMARY KEY,
                  value_json TEXT NOT NULL
                );
                CREATE TABLE messages (
                  session_id TEXT NOT NULL,
                  position INTEGER NOT NULL CHECK(position >= 0),
                  message_id TEXT NOT NULL,
                  value_json TEXT NOT NULL,
                  overview_kind INTEGER NOT NULL DEFAULT 0,
                  is_user INTEGER NOT NULL DEFAULT 0,
                  PRIMARY KEY(session_id, position),
                  UNIQUE(session_id, message_id),
                  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
                ) WITHOUT ROWID;
                CREATE TABLE session_overviews (
                  session_id TEXT PRIMARY KEY,
                  value_blob BLOB NOT NULL,
                  FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
                ) WITHOUT ROWID;
                ",
            )
            .expect("mixed overview fixture should initialize");
        seed_current_state_auxiliary_tables(&connection);
        seed_current_state_metadata(&connection);

        let candidates: [(&str, rusqlite::types::Value); 10] = [
            ("healthy-local", "{}".to_owned().into()),
            (
                "valid-remote",
                r#"{"remoteId":"r","remoteSessionId":"s"}"#
                    .to_owned()
                    .into(),
            ),
            (
                "partial-remote",
                r#"{"remoteId":"r"}"#.to_owned().into(),
            ),
            (
                "partial-remote-session",
                r#"{"remoteSessionId":"s"}"#.to_owned().into(),
            ),
            (
                "empty-remote",
                r#"{"remoteId":"","remoteSessionId":"s"}"#
                    .to_owned()
                    .into(),
            ),
            (
                "empty-remote-session",
                r#"{"remoteId":"r","remoteSessionId":" "}"#
                    .to_owned()
                    .into(),
            ),
            (
                "non-string-remote",
                r#"{"remoteId":7}"#.to_owned().into(),
            ),
            ("malformed-json", "{".to_owned().into()),
            ("non-object", "[]".to_owned().into()),
            (
                "unreadable-storage-class",
                rusqlite::types::Value::Blob(vec![0xff, 0xfe]),
            ),
        ];
        for (session_id, metadata) in candidates {
            connection
                .execute(
                    "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)",
                    rusqlite::params![session_id, metadata],
                )
                .expect("overview candidate should insert");
            connection
                .execute(
                    "INSERT INTO messages(
                         session_id,
                         position,
                         message_id,
                         value_json,
                         overview_kind,
                         is_user
                     )
                     VALUES(?1, 0, ?2, '{}', 0, 0)",
                    rusqlite::params![session_id, format!("message-{session_id}")],
                )
                .expect("overview candidate message should insert");
        }

        ensure_sqlite_state_schema(&connection)
            .expect("invalid overview candidates must not abort global schema startup");

        let healthy_blob: Vec<u8> = connection
            .query_row(
                "SELECT value_blob
                 FROM session_overviews
                 WHERE session_id = 'healthy-local'",
                [],
                |row| row.get(0),
            )
            .expect("healthy local overview should backfill");
        assert_eq!(
            healthy_blob,
            vec![encode_conversation_overview_message(
                ConversationOverviewKind::Text,
                false,
            )]
        );
        let invalid_overview_count: u32 = connection
            .query_row(
                "SELECT COUNT(*)
                 FROM session_overviews
                 WHERE session_id != 'healthy-local'",
                [],
                |row| row.get(0),
            )
            .expect("invalid overview count should be queryable");
        assert_eq!(
            invalid_overview_count, 0,
            "remote and invalid sessions must not gain local overview blobs"
        );
    }
}
