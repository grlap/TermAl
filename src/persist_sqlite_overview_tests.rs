// Retained overview storage must be inert during schema initialization.

mod sqlite_overview_tests {
    use super::*;

    #[test]
    fn current_schema_preserves_historical_overviews_without_scanning_session_metadata() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        seed_current_state_core_tables(&connection);
        seed_current_state_auxiliary_tables(&connection);
        seed_current_state_metadata(&connection);

        let candidates: [(&str, rusqlite::types::Value); 10] = [
            ("healthy-local", "{}".to_owned().into()),
            (
                "valid-remote",
                r#"{"remoteId":"r","remoteSessionId":"s"}"#.to_owned().into(),
            ),
            ("partial-remote", r#"{"remoteId":"r"}"#.to_owned().into()),
            (
                "partial-remote-session",
                r#"{"remoteSessionId":"s"}"#.to_owned().into(),
            ),
            (
                "empty-remote",
                r#"{"remoteId":"","remoteSessionId":"s"}"#.to_owned().into(),
            ),
            (
                "empty-remote-session",
                r#"{"remoteId":"r","remoteSessionId":" "}"#.to_owned().into(),
            ),
            ("non-string-remote", r#"{"remoteId":7}"#.to_owned().into()),
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

        let historical = vec![255_u8, 7, 2];
        connection
            .execute(
                "INSERT INTO session_overviews(session_id,value_blob) VALUES('valid-remote',?1)",
                [&historical],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE messages SET overview_kind=93,is_user=7 WHERE session_id='healthy-local'",
                [],
            )
            .unwrap();
        connection.execute_batch(
            "CREATE TRIGGER no_overview_insert BEFORE INSERT ON session_overviews BEGIN
               SELECT RAISE(ABORT,'overview insertion is retired'); END;
             CREATE TRIGGER no_overview_update BEFORE UPDATE ON session_overviews BEGIN
               SELECT RAISE(ABORT,'overview update is retired'); END;
             CREATE TRIGGER no_overview_delete BEFORE DELETE ON session_overviews BEGIN
               SELECT RAISE(ABORT,'historical overview must remain'); END;
             CREATE TRIGGER no_overview_classification BEFORE UPDATE OF overview_kind,is_user ON messages BEGIN
               SELECT RAISE(ABORT,'message classification is retired'); END;"
        ).unwrap();
        for _ in 0..2 {
            ensure_sqlite_state_schema(&connection).expect(
                "unreadable or invalid session metadata must not enter an overview backfill",
            );
        }
        let blobs: Vec<(String, Vec<u8>)> = connection
            .prepare("SELECT session_id,value_blob FROM session_overviews")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(blobs, vec![("valid-remote".into(), historical)]);
        let metadata: (i64, i64) = connection
            .query_row(
                "SELECT overview_kind,is_user FROM messages WHERE session_id='healthy-local'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            metadata,
            (93, 7),
            "even obsolete values remain uninterpreted"
        );
    }
}
