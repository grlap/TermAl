mod sqlite_message_storage_tests {
    use super::*;

    const PREVIOUS_MESSAGES_DDL: &str = "
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
        ) WITHOUT ROWID;";

    fn connection(previous_layout: bool) -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        ensure_sqlite_state_schema(&connection).unwrap();
        if previous_layout {
            connection.execute_batch("DROP TABLE messages;").unwrap();
            connection.execute_batch(PREVIOUS_MESSAGES_DDL).unwrap();
        }
        connection
    }

    fn without_rowid(connection: &rusqlite::Connection) -> bool {
        connection
            .query_row(
                "SELECT wr FROM pragma_table_list('messages') WHERE schema = 'main'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn snapshot(connection: &rusqlite::Connection) -> Vec<(String, i64, String, String, i64, i64)> {
        connection
            .prepare(
                "SELECT session_id, position, message_id, value_json, overview_kind, is_user
             FROM messages ORDER BY session_id, position",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn install_audit(connection: &rusqlite::Connection) {
        connection
            .execute_batch(
                "CREATE TABLE message_write_audit(operation TEXT, message_id TEXT);
             CREATE TRIGGER audit_message_insert AFTER INSERT ON messages BEGIN
               INSERT INTO message_write_audit VALUES('insert', new.message_id); END;
             CREATE TRIGGER audit_message_update AFTER UPDATE ON messages BEGIN
               INSERT INTO message_write_audit VALUES('update', new.message_id); END;
             CREATE TRIGGER audit_message_delete AFTER DELETE ON messages BEGIN
               INSERT INTO message_write_audit VALUES('delete', old.message_id); END;",
            )
            .unwrap();
    }

    fn audit(connection: &rusqlite::Connection) -> Vec<(String, String)> {
        connection
            .prepare("SELECT operation, message_id FROM message_write_audit ORDER BY rowid")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn session(count: usize) -> SerializedPersistedSession {
        SerializedPersistedSession {
            session_id: "session-test".into(),
            message_start_index: 0,
            message_count: count,
            write_overview: true,
            prompt_history_value_json: None,
            value_json: "{}".into(),
            messages: (0..count)
                .map(|position| SerializedPersistedMessage {
                    position,
                    message_id: format!("message-{position}"),
                    value_json: json!({"text": "x".repeat(128 * 1024), "position": position})
                        .to_string(),
                    overview_kind: ConversationOverviewKind::Text,
                    is_user: false,
                })
                .collect(),
        }
    }

    fn save(connection: &mut rusqlite::Connection, session: &SerializedPersistedSession) {
        let tx = connection.transaction().unwrap();
        write_serialized_persisted_session(&tx, session).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn fresh_messages_keep_large_bodies_out_of_the_primary_key_tree() {
        assert!(!without_rowid(&connection(false)));
    }

    #[test]
    fn conversion_restores_views_before_their_instead_of_triggers() {
        let connection = connection(true);
        connection.execute_batch(
            "INSERT INTO sessions VALUES('session-a', '{}');
             INSERT INTO messages VALUES('session-a', 0, 'first', '{}', 0, 0);
             CREATE VIEW message_ids AS SELECT message_id FROM messages;
             CREATE TRIGGER delete_message_view INSTEAD OF DELETE ON message_ids BEGIN
               DELETE FROM messages WHERE message_id = old.message_id; END;
             CREATE TABLE unrelated_values(value TEXT);
             INSERT INTO unrelated_values VALUES('kept');
             CREATE VIEW unrelated_view AS SELECT value FROM unrelated_values;
             CREATE TRIGGER delete_unrelated_view INSTEAD OF DELETE ON unrelated_view BEGIN
               DELETE FROM unrelated_values WHERE value = old.value; END;",
        ).unwrap();
        let before = snapshot(&connection);
        ensure_sqlite_state_schema(&connection).unwrap();
        assert!(!without_rowid(&connection));
        assert_eq!(snapshot(&connection), before);
        connection.execute_batch(
            "DELETE FROM message_ids WHERE message_id = 'first';
             DELETE FROM unrelated_view WHERE value = 'kept';",
        ).unwrap();
        assert!(snapshot(&connection).is_empty());
        assert_eq!(
            connection.query_row("SELECT count(*) FROM unrelated_values", [], |row| row.get::<_, i64>(0)).unwrap(),
            0
        );
        assert!(!connection.prepare("PRAGMA foreign_key_check").unwrap().exists([]).unwrap());
    }

    #[test]
    fn conversion_preserves_records_schema_objects_and_foreign_keys() {
        let connection = connection(true);
        connection.execute_batch(
            "INSERT INTO sessions VALUES('session-a', '{}');
             INSERT INTO messages VALUES('session-a', 0, 'first', '{invalid but preserved', 3, 1);
             INSERT INTO messages VALUES('session-a', 1, 'second', '{}', 0, 0);
             CREATE INDEX message_author_index ON messages(session_id, is_user);
             CREATE VIEW message_ids AS SELECT message_id FROM messages;
             CREATE TABLE message_references(
               session_id TEXT, message_id TEXT,
               FOREIGN KEY(session_id, message_id) REFERENCES messages(session_id, message_id) ON DELETE CASCADE
             );
             INSERT INTO message_references VALUES('session-a', 'first');",
        ).unwrap();
        install_audit(&connection);
        let before = snapshot(&connection);
        ensure_sqlite_state_schema(&connection).unwrap();
        assert!(!without_rowid(&connection));
        assert_eq!(snapshot(&connection), before);
        assert!(
            audit(&connection).is_empty(),
            "conversion must not fire application write triggers"
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM message_references", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM message_ids", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        connection.prepare("SELECT message_id FROM messages INDEXED BY message_author_index WHERE session_id = ?1").unwrap();
        assert!(
            connection
                .execute(
                    "INSERT INTO messages VALUES('session-a', 2, 'first', '{}', 0, 0)",
                    []
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO messages VALUES('session-a', 0, 'third', '{}', 0, 0)",
                    []
                )
                .is_err()
        );
        let changes = connection.total_changes();
        ensure_sqlite_state_schema(&connection).unwrap();
        assert_eq!(
            connection.total_changes(),
            changes,
            "reopening must not repeat conversion"
        );
        connection
            .execute("DELETE FROM sessions WHERE id = 'session-a'", [])
            .unwrap();
        assert!(snapshot(&connection).is_empty());
        assert_eq!(
            audit(&connection).len(),
            2,
            "write triggers must still work after conversion"
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM message_references", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn failed_conversion_rolls_back_original_records_and_restores_foreign_keys() {
        let connection = connection(true);
        connection
            .execute_batch(
                "PRAGMA foreign_keys = OFF;
             INSERT INTO messages VALUES('missing-parent', 0, 'keep-me', '{}', 0, 0);
             PRAGMA foreign_keys = ON;",
            )
            .unwrap();
        let before = snapshot(&connection);
        let error = ensure_sqlite_state_schema(&connection).unwrap_err();
        assert!(format!("{error:#}").contains("foreign key"));
        assert!(without_rowid(&connection));
        assert_eq!(snapshot(&connection), before);
        assert_eq!(
            connection
                .query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name = 'messages_rowid_rebuild'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn metadata_saves_and_streaming_updates_preserve_unchanged_message_rows() {
        let mut connection = connection(false);
        let mut session = session(8);
        save(&mut connection, &session);
        let before = snapshot(&connection);
        install_audit(&connection);
        session.value_json = "{\"preview\":\"changed metadata\"}".into();
        save(&mut connection, &session);
        assert_eq!(snapshot(&connection), before);
        assert!(
            audit(&connection).is_empty(),
            "metadata-only saves must not rewrite transcript rows"
        );
        session.messages[7].value_json = "{\"text\":\"stream advanced\"}".into();
        session.messages[7].overview_kind = ConversationOverviewKind::Error;
        session.messages[7].is_user = true;
        save(&mut connection, &session);
        assert_eq!(audit(&connection), [("update".into(), "message-7".into())]);
        let after = snapshot(&connection);
        assert_eq!(&after[..7], &before[..7]);
        assert_eq!(after[7].3, session.messages[7].value_json);
        assert_eq!((after[7].4, after[7].5), (3, 1));
    }

    #[test]
    fn differential_writes_support_reordering_truncation_append_and_cold_prefixes() {
        let mut connection = connection(false);
        let mut session = session(5);
        save(&mut connection, &session);
        let prefix = snapshot(&connection)[0].clone();
        session.messages.remove(0);
        session.message_start_index = 1;
        session.messages.swap(0, 1);
        for (offset, message) in session.messages.iter_mut().enumerate() {
            message.position = offset + 1;
        }
        session.messages.pop();
        session.message_count = 4;
        save(&mut connection, &session);
        let rows = snapshot(&connection);
        assert_eq!(rows[0], prefix);
        assert_eq!(
            rows.iter().map(|row| row.2.as_str()).collect::<Vec<_>>(),
            ["message-0", "message-2", "message-1", "message-3"]
        );
        session.messages.push(SerializedPersistedMessage {
            position: 4,
            message_id: "appended".into(),
            value_json: "{}".into(),
            overview_kind: ConversationOverviewKind::Text,
            is_user: false,
        });
        session.message_count = 5;
        save(&mut connection, &session);
        assert_eq!(snapshot(&connection)[4].2, "appended");
        session.messages.clear();
        session.message_count = 1;
        save(&mut connection, &session);
        assert_eq!(snapshot(&connection), [prefix]);
    }

    #[test]
    fn failed_tail_update_keeps_previous_transcript_and_overview_atomic() {
        let mut connection = connection(false);
        let mut session = session(4);
        save(&mut connection, &session);
        let before = snapshot(&connection);
        let overview: Vec<u8> = connection
            .query_row("SELECT value_blob FROM session_overviews", [], |r| r.get(0))
            .unwrap();
        session.messages.remove(0);
        session.message_start_index = 1;
        session.messages[1].message_id = "message-0".into();
        {
            let tx = connection.transaction().unwrap();
            assert!(write_serialized_persisted_session(&tx, &session).is_err());
        }
        assert_eq!(snapshot(&connection), before);
        assert_eq!(
            connection
                .query_row("SELECT value_blob FROM session_overviews", [], |r| r
                    .get::<_, Vec<u8>>(0))
                .unwrap(),
            overview
        );
    }

    #[test]
    fn rejected_startup_authority_does_not_convert_existing_messages() {
        let connection = connection(true);
        connection
            .execute(
                "INSERT INTO app_state VALUES(?1, ?2)",
                rusqlite::params![SQLITE_METADATA_KEY, "{invalid metadata"],
            )
            .unwrap();
        let changes = connection.total_changes();
        assert!(ensure_sqlite_state_schema_for_load(&connection).is_err());
        assert!(without_rowid(&connection));
        assert_eq!(connection.total_changes(), changes);
    }

    #[test]
    #[ignore = "explicit disk benchmark comparing previous and current transcript persistence"]
    fn benchmark_large_transcript_persistence() {
        let root = TestTempRoot::create("termal-message-storage-benchmark");
        let mut expected = None;
        for previous in [true, false] {
            let path = root.path().join(if previous {
                "previous.sqlite"
            } else {
                "current.sqlite"
            });
            let mut connection = rusqlite::Connection::open(path).unwrap();
            ensure_sqlite_state_schema(&connection).unwrap();
            if previous {
                connection.execute_batch("DROP TABLE messages;").unwrap();
                connection.execute_batch(PREVIOUS_MESSAGES_DDL).unwrap();
            }
            let mut target = session(64);
            for id in ["session-a", "session-b", "session-c"] {
                target.session_id = id.into();
                save(&mut connection, &target);
            }
            target.session_id = "session-b".into();
            let changes = connection.total_changes();
            let started = std::time::Instant::now();
            for update in 0..12 {
                target.messages[63].value_json =
                    json!({"text": "y".repeat(128 * 1024), "update": update}).to_string();
                let tx = connection.transaction().unwrap();
                if previous {
                    tx.execute(
                        "DELETE FROM messages WHERE session_id = ?1 AND position >= 0",
                        [&target.session_id],
                    )
                    .unwrap();
                    let mut insert = tx.prepare_cached(
                        "INSERT INTO messages(session_id, position, message_id, value_json, overview_kind, is_user)
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    ).unwrap();
                    for message in &target.messages {
                        insert
                            .execute(rusqlite::params![
                                target.session_id,
                                message.position as i64,
                                message.message_id,
                                message.value_json,
                                0,
                                0
                            ])
                            .unwrap();
                    }
                } else {
                    write_serialized_persisted_messages(&tx, &target).unwrap();
                }
                tx.commit().unwrap();
            }
            eprintln!(
                "transcript benchmark: previous={previous}, updates=12, elapsed_ms={}, message_writes={}",
                started.elapsed().as_millis(),
                connection.total_changes() - changes
            );
            let rows = snapshot(&connection);
            if previous {
                // Exercise the real conversion on the same large disk fixture,
                // including every overflow payload, after timing the old writes.
                let started = std::time::Instant::now();
                ensure_sqlite_state_schema(&connection).unwrap();
                eprintln!(
                    "transcript benchmark: conversion_ms={}",
                    started.elapsed().as_millis()
                );
                assert!(!without_rowid(&connection));
                assert_eq!(snapshot(&connection), rows);
                expected = Some(rows);
            } else {
                assert_eq!(rows, expected.take().unwrap());
                assert_eq!(connection.total_changes() - changes, 12);
            }
        }
    }
}
