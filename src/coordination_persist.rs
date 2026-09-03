/*
Current coordination SQLite schema ownership.

Owns the independent `coordination.sqlite` path plus current-schema
initialization and validation. Runtime mailbox and board stores use these
helpers but keep separate long-lived connections and writer-admission domains.
Unreleased legacy schemas are rejected with reset guidance rather than
migrated or copied from `termal.sqlite`.
*/

fn resolve_coordination_persistence_path(persistence_path: &FsPath) -> PathBuf {
    persistence_path.with_file_name("coordination.sqlite")
}

const COORDINATION_SQLITE_SCHEMA_VERSION: &str = "1";

const CURRENT_COORDINATION_TABLE_COLUMNS: &[(&str, &[&str])] = &[
    ("meta", &["key", "value"]),
    (
        "mailboxes",
        &["id", "participant_key", "created_at", "next_sequence"],
    ),
    (
        "mailbox_participants",
        &[
            "mailbox_id",
            "session_id",
            "display_name",
            "processed_through",
            "joined_at",
            "left_at",
        ],
    ),
    (
        "mailbox_messages",
        &[
            "id",
            "mailbox_id",
            "sequence",
            "sender_session_id",
            "sender_name",
            "target_session_id",
            "target_name",
            "created_at",
            "class",
            "topic",
            "state_stamp",
            "body",
            "idempotency_key",
            "unread_depth_at_append",
            "notification_disposition",
            "dispatch_outcome",
        ],
    ),
    (
        "coordination_board_scopes",
        &["scope_id", "generation"],
    ),
    (
        "coordination_board_entries",
        &[
            "scope_id",
            "key",
            "revision",
            "generation",
            "value_json",
            "author_session_id",
            "author_name",
            "updated_at",
            "state_stamp",
        ],
    ),
    (
        "coordination_board_history",
        &[
            "scope_id",
            "key",
            "revision",
            "generation",
            "value_json",
            "author_session_id",
            "author_name",
            "updated_at",
            "state_stamp",
        ],
    ),
    (
        "coordination_board_idempotency",
        &[
            "scope_id",
            "author_session_id",
            "idempotency_key",
            "request_hash",
            "receipt_json",
            "created_at",
        ],
    ),
    (
        "coordination_board_deleted_scopes",
        &["scope_id", "deleted_at"],
    ),
];

fn coordination_user_table_names(
    connection: &rusqlite::Connection,
) -> Result<BTreeSet<String>> {
    let mut statement = connection
        .prepare(
            "SELECT name
             FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .context("failed to inspect coordination database tables")?;
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .context("failed to query coordination database tables")?
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .context("failed to decode coordination database tables")
}

fn coordination_table_columns(
    connection: &rusqlite::Connection,
    table_name: &str,
) -> Result<Vec<String>> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table_name})"))
        .with_context(|| format!("failed to inspect coordination table `{table_name}`"))?;
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .with_context(|| format!("failed to query coordination table `{table_name}`"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .with_context(|| format!("failed to decode coordination table `{table_name}`"))
}

fn initialize_current_coordination_schema(connection: &rusqlite::Connection) -> Result<()> {
    let transaction = rusqlite::Transaction::new_unchecked(
        connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .context("failed to begin current coordination schema initialization")?;
    transaction
        .execute_batch(
            "
            CREATE TABLE meta (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL
            );

            CREATE TABLE mailboxes (
              id TEXT PRIMARY KEY,
              participant_key TEXT NOT NULL UNIQUE,
              created_at TEXT NOT NULL,
              next_sequence INTEGER NOT NULL
            );

            CREATE TABLE mailbox_participants (
              mailbox_id TEXT NOT NULL,
              session_id TEXT NOT NULL,
              display_name TEXT NOT NULL,
              processed_through INTEGER NOT NULL DEFAULT 0,
              joined_at TEXT NOT NULL,
              left_at TEXT,
              PRIMARY KEY (mailbox_id, session_id),
              FOREIGN KEY (mailbox_id) REFERENCES mailboxes(id) ON DELETE CASCADE
            );

            CREATE TABLE mailbox_messages (
              id TEXT PRIMARY KEY,
              mailbox_id TEXT NOT NULL,
              sequence INTEGER NOT NULL,
              sender_session_id TEXT NOT NULL,
              sender_name TEXT NOT NULL,
              target_session_id TEXT NOT NULL,
              target_name TEXT NOT NULL,
              created_at TEXT NOT NULL,
              class TEXT NOT NULL CHECK (class = 'routine'),
              topic TEXT,
              state_stamp TEXT,
              body TEXT NOT NULL,
              idempotency_key TEXT NOT NULL,
              unread_depth_at_append INTEGER NOT NULL,
              notification_disposition TEXT NOT NULL,
              dispatch_outcome TEXT,
              UNIQUE (mailbox_id, sequence),
              UNIQUE (sender_session_id, idempotency_key),
              FOREIGN KEY (mailbox_id) REFERENCES mailboxes(id) ON DELETE CASCADE
            );

            CREATE INDEX mailbox_participants_session
              ON mailbox_participants(session_id, left_at);

            CREATE TABLE coordination_board_scopes (
              scope_id TEXT PRIMARY KEY,
              generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0)
            );

            CREATE TABLE coordination_board_entries (
              scope_id TEXT NOT NULL,
              key TEXT NOT NULL,
              revision INTEGER NOT NULL CHECK (revision > 0),
              generation INTEGER NOT NULL CHECK (generation > 0),
              value_json TEXT,
              author_session_id TEXT NOT NULL,
              author_name TEXT NOT NULL,
              updated_at TEXT NOT NULL,
              state_stamp TEXT,
              PRIMARY KEY (scope_id, key),
              FOREIGN KEY (scope_id)
                REFERENCES coordination_board_scopes(scope_id)
                ON DELETE CASCADE
            );

            CREATE TABLE coordination_board_history (
              scope_id TEXT NOT NULL,
              key TEXT NOT NULL,
              revision INTEGER NOT NULL CHECK (revision > 0),
              generation INTEGER NOT NULL CHECK (generation > 0),
              value_json TEXT,
              author_session_id TEXT NOT NULL,
              author_name TEXT NOT NULL,
              updated_at TEXT NOT NULL,
              state_stamp TEXT,
              PRIMARY KEY (scope_id, key, revision),
              FOREIGN KEY (scope_id)
                REFERENCES coordination_board_scopes(scope_id)
                ON DELETE CASCADE
            );

            CREATE TABLE coordination_board_idempotency (
              scope_id TEXT NOT NULL,
              author_session_id TEXT NOT NULL,
              idempotency_key TEXT NOT NULL,
              request_hash TEXT NOT NULL,
              receipt_json TEXT NOT NULL,
              created_at TEXT NOT NULL,
              PRIMARY KEY (scope_id, author_session_id, idempotency_key),
              FOREIGN KEY (scope_id)
                REFERENCES coordination_board_scopes(scope_id)
                ON DELETE CASCADE
            );

            CREATE TABLE coordination_board_deleted_scopes (
              scope_id TEXT PRIMARY KEY,
              deleted_at TEXT NOT NULL
            );
            ",
        )
        .context("failed to initialize current SQLite coordination schema")?;
    transaction
        .execute(
            "INSERT INTO meta(key, value) VALUES('coordination_schema_version', ?1)",
            rusqlite::params![COORDINATION_SQLITE_SCHEMA_VERSION],
        )
        .context("failed to record SQLite coordination schema version")?;
    transaction
        .commit()
        .context("failed to commit current coordination schema initialization")
}

fn reject_unsupported_coordination_schema(detail: impl std::fmt::Display) -> anyhow::Error {
    anyhow!(
        "unsupported coordination database schema ({detail}); this unreleased local state is not migrated. \
         Move or delete `coordination.sqlite` to reset mailboxes and coordination boards, then restart TermAl"
    )
}

fn validate_current_coordination_schema(connection: &rusqlite::Connection) -> Result<()> {
    let expected_tables = CURRENT_COORDINATION_TABLE_COLUMNS
        .iter()
        .map(|(table_name, _)| (*table_name).to_owned())
        .collect::<BTreeSet<_>>();
    let actual_tables = coordination_user_table_names(connection)?;
    if actual_tables != expected_tables {
        return Err(reject_unsupported_coordination_schema(format!(
            "expected tables {expected_tables:?}, found {actual_tables:?}"
        )));
    }

    let stored_schema_version = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'coordination_schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|err| {
            reject_unsupported_coordination_schema(format!(
                "missing readable coordination schema version: {err}"
            ))
        })?;
    if stored_schema_version != COORDINATION_SQLITE_SCHEMA_VERSION {
        return Err(reject_unsupported_coordination_schema(format!(
            "found version `{stored_schema_version}`, expected `{COORDINATION_SQLITE_SCHEMA_VERSION}`"
        )));
    }

    for (table_name, expected_columns) in CURRENT_COORDINATION_TABLE_COLUMNS {
        let actual_columns = coordination_table_columns(connection, table_name)?;
        let expected_columns = expected_columns
            .iter()
            .map(|column_name| (*column_name).to_owned())
            .collect::<Vec<_>>();
        if actual_columns != expected_columns {
            return Err(reject_unsupported_coordination_schema(format!(
                "table `{table_name}` expected columns {expected_columns:?}, found {actual_columns:?}"
            )));
        }
    }
    Ok(())
}

fn ensure_sqlite_coordination_schema(connection: &rusqlite::Connection) -> Result<()> {
    if coordination_user_table_names(connection)?.is_empty() {
        initialize_current_coordination_schema(connection)?;
    }
    validate_current_coordination_schema(connection)
}

fn ensure_sqlite_coordination_schema_for_path(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<()> {
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    ensure_sqlite_coordination_schema(connection)
}

fn bootstrap_coordination_database(
    coordination_path: &FsPath,
) -> Result<rusqlite::Connection> {
    if let Some(parent) = coordination_path.parent() {
        create_local_state_directory(parent)?;
    }
    reject_existing_sqlite_state_path_redirection(coordination_path)?;
    let connection = open_sqlite_state_connection(coordination_path)?;
    ensure_sqlite_coordination_schema_for_path(&connection, coordination_path)?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .with_context(|| {
            format!(
                "failed to enable coordination foreign keys for `{}`",
                coordination_path.display()
            )
        })?;
    verify_persist_commit_integrity(coordination_path)?;
    Ok(connection)
}

#[cfg(test)]
mod sqlite_coordination_tests {
    use super::*;

    fn expected_coordination_tables() -> BTreeSet<String> {
        CURRENT_COORDINATION_TABLE_COLUMNS
            .iter()
            .map(|(table_name, _)| (*table_name).to_owned())
            .collect()
    }

    fn assert_reset_guidance(error: &anyhow::Error) {
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("unsupported coordination database schema"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Move or delete `coordination.sqlite`"),
            "{rendered}"
        );
        assert!(rendered.contains("not migrated"), "{rendered}");
    }

    #[test]
    fn empty_coordination_database_initializes_only_the_current_schema() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");

        ensure_sqlite_coordination_schema(&connection)
            .expect("empty database should initialize the current schema");

        assert_eq!(
            coordination_user_table_names(&connection).expect("table inventory should read"),
            expected_coordination_tables()
        );
        validate_current_coordination_schema(&connection)
            .expect("fresh current schema should validate");
        let metadata = connection
            .prepare("SELECT key, value FROM meta ORDER BY key")
            .expect("metadata query should prepare")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("metadata query should run")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("metadata should decode");
        assert_eq!(
            metadata,
            vec![(
                "coordination_schema_version".to_owned(),
                COORDINATION_SQLITE_SCHEMA_VERSION.to_owned()
            )],
            "fresh databases contain only the current schema version marker"
        );
    }

    #[test]
    fn current_coordination_schema_validation_is_read_only() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_coordination_schema(&connection)
            .expect("current schema should initialize");
        connection
            .execute_batch("PRAGMA query_only = ON;")
            .expect("test connection should enter query-only mode");

        ensure_sqlite_coordination_schema(&connection)
            .expect("current schema checks must not require a write");
    }

    #[test]
    fn current_coordination_schema_initialization_is_safe_across_connections() {
        let root = std::env::temp_dir().join(format!(
            "termal-current-coordination-schema-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("test directory should exist");
        let path = root.join("coordination.sqlite");
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let connection = open_sqlite_state_connection(&path)
                        .expect("concurrent SQLite connection should open");
                    barrier.wait();
                    ensure_sqlite_coordination_schema_for_path(&connection, &path)
                        .expect("concurrent current-schema ensure should succeed");
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().expect("schema ensure thread should join");
        }

        let connection =
            rusqlite::Connection::open(&path).expect("schema database should reopen");
        validate_current_coordination_schema(&connection)
            .expect("concurrently initialized schema should be current");
        let metadata_count: u32 = connection
            .query_row("SELECT COUNT(*) FROM meta", [], |row| row.get(0))
            .expect("metadata count should read");
        assert_eq!(metadata_count, 1);
        drop(connection);
        fs::remove_dir_all(root).expect("test directory should clean up");
    }

    #[test]
    fn obsolete_coordination_schema_is_rejected_without_backfill() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_coordination_schema(&connection)
            .expect("current schema should initialize");
        connection
            .execute_batch("ALTER TABLE mailbox_messages DROP COLUMN dispatch_outcome;")
            .expect("fixture should remove the current immutable outcome column");
        let before = coordination_table_columns(&connection, "mailbox_messages")
            .expect("obsolete columns should read");

        let error = ensure_sqlite_coordination_schema(&connection)
            .expect_err("obsolete coordination schema must be rejected");
        assert_reset_guidance(&error);
        assert_eq!(
            coordination_table_columns(&connection, "mailbox_messages")
                .expect("rejected columns should remain readable"),
            before,
            "schema validation must not mutate or backfill obsolete state"
        );
    }

    #[test]
    fn unsupported_coordination_schema_version_is_rejected_without_rewrite() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_coordination_schema(&connection)
            .expect("current schema should initialize");
        connection
            .execute(
                "UPDATE meta SET value = '0' WHERE key = 'coordination_schema_version'",
                [],
            )
            .expect("fixture should install an unsupported version");
        connection
            .execute_batch("PRAGMA query_only = ON;")
            .expect("test connection should enter query-only mode");

        let error = ensure_sqlite_coordination_schema(&connection)
            .expect_err("unsupported version must be rejected");
        assert_reset_guidance(&error);
        let stored_version: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'coordination_schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("rejected version should remain readable");
        assert_eq!(stored_version, "0");
    }

    #[test]
    fn coordination_bootstrap_creates_and_reopens_only_the_current_database() {
        let root = std::env::temp_dir().join(format!(
            "termal-current-coordination-bootstrap-{}",
            Uuid::new_v4()
        ));
        let path = root.join("coordination.sqlite");

        bootstrap_coordination_database(&path)
            .expect("bootstrap should initialize a fresh current database");
        bootstrap_coordination_database(&path)
            .expect("bootstrap should reopen an existing current database");

        let connection =
            rusqlite::Connection::open(&path).expect("bootstrapped database should reopen");
        validate_current_coordination_schema(&connection)
            .expect("bootstrapped database should remain current");
        drop(connection);
        fs::remove_dir_all(root).expect("test directory should clean up");
    }
}
