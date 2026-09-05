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

// Compatibility contract: SQLite preserves the schema SQL used to create each
// object, and validation below compares that stored SQL with this canonical
// text after collapsing whitespace runs. The resulting token sequence is an
// on-disk compatibility surface. Any edit that changes it, including cosmetic
// punctuation spacing, must be paired with a schema-version bump and an
// explicit migration or reset decision.
const CURRENT_COORDINATION_SCHEMA_SQL: &str = "
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
";

#[derive(Clone, Debug, Eq, PartialEq)]
struct CoordinationSchemaObject {
    object_type: String,
    name: String,
    table_name: String,
    sql: Option<String>,
}

fn coordination_schema_objects(
    connection: &rusqlite::Connection,
) -> Result<Vec<CoordinationSchemaObject>> {
    let mut statement = connection
        .prepare(
            "SELECT type, name, tbl_name, sql
             FROM sqlite_schema
             WHERE type IN ('table', 'index', 'view', 'trigger')
               AND name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
        )
        .context("failed to inspect coordination database schema objects")?;
    statement
        .query_map([], |row| {
            Ok(CoordinationSchemaObject {
                object_type: row.get(0)?,
                name: row.get(1)?,
                table_name: row.get(2)?,
                sql: row
                    .get::<_, Option<String>>(3)?
                    .map(|sql| sql.split_whitespace().collect::<Vec<_>>().join(" ")),
            })
        })
        .context("failed to query coordination database schema objects")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to decode coordination database schema objects")
}

fn expected_coordination_schema_objects() -> Result<Vec<CoordinationSchemaObject>> {
    let connection = rusqlite::Connection::open_in_memory()
        .context("failed to open the canonical coordination schema database")?;
    connection
        .execute_batch(CURRENT_COORDINATION_SCHEMA_SQL)
        .context("failed to build the canonical coordination schema")?;
    coordination_schema_objects(&connection)
}

fn initialize_current_coordination_schema(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    transaction
        .execute_batch(CURRENT_COORDINATION_SCHEMA_SQL)
        .context("failed to initialize current SQLite coordination schema")?;
    transaction
        .execute(
            "INSERT INTO meta(key, value) VALUES('coordination_schema_version', ?1)",
            rusqlite::params![COORDINATION_SQLITE_SCHEMA_VERSION],
        )
        .context("failed to record SQLite coordination schema version")?;
    Ok(())
}

fn reject_unsupported_coordination_schema(detail: impl std::fmt::Display) -> anyhow::Error {
    anyhow!(
        "unsupported coordination database schema ({detail}); this unreleased local state is not migrated. \
         Move or delete that coordination database to reset mailboxes and coordination boards, then restart TermAl"
    )
}

fn validate_current_coordination_schema(connection: &rusqlite::Connection) -> Result<()> {
    let expected_schema = expected_coordination_schema_objects()?;
    let actual_schema = coordination_schema_objects(connection)?;
    let expected_inventory = expected_schema
        .iter()
        .map(|object| format!("{} `{}`", object.object_type, object.name))
        .collect::<BTreeSet<_>>();
    let actual_inventory = actual_schema
        .iter()
        .map(|object| format!("{} `{}`", object.object_type, object.name))
        .collect::<BTreeSet<_>>();
    if actual_inventory != expected_inventory {
        return Err(reject_unsupported_coordination_schema(format!(
            "expected schema objects {expected_inventory:?}, found {actual_inventory:?}"
        )));
    }
    if actual_schema != expected_schema {
        let changed_object = expected_schema
            .iter()
            .zip(&actual_schema)
            .find(|(expected, actual)| expected != actual)
            .map(|(expected, _)| format!("{} `{}`", expected.object_type, expected.name))
            .unwrap_or_else(|| "unknown schema object".to_owned());
        return Err(reject_unsupported_coordination_schema(format!(
            "definition for {changed_object} differs from the current schema"
        )));
    }

    let stored_schema_version = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'coordination_schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to read the coordination schema version")?
        .ok_or_else(|| {
            reject_unsupported_coordination_schema("missing coordination schema version")
        })?;
    if stored_schema_version != COORDINATION_SQLITE_SCHEMA_VERSION {
        return Err(reject_unsupported_coordination_schema(format!(
            "found version `{stored_schema_version}`, expected `{COORDINATION_SQLITE_SCHEMA_VERSION}`"
        )));
    }
    Ok(())
}

fn ensure_sqlite_coordination_schema(connection: &rusqlite::Connection) -> Result<()> {
    if !coordination_schema_objects(connection)?.is_empty() {
        return validate_current_coordination_schema(connection);
    }

    let transaction =
        rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)
            .context("failed to begin current coordination schema initialization")?;
    if coordination_schema_objects(&transaction)?.is_empty() {
        initialize_current_coordination_schema(&transaction)?;
    }
    validate_current_coordination_schema(&transaction)?;
    transaction
        .commit()
        .context("failed to commit current coordination schema initialization")
}

fn ensure_sqlite_coordination_schema_for_path(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<()> {
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    // This connection-local setting must precede the initialization transaction.
    // Persistent PRAGMAs still wait until the current-schema guard succeeds.
    let setup = (|| {
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .context("failed to enable coordination foreign keys")?;
        ensure_sqlite_coordination_schema(connection)
    })().with_context(|| {
        format!(
            "failed to open or validate coordination database `{}`",
            path.display()
        )
    });
    finish_sqlite_state_file_setup(path, setup)
}

fn bootstrap_coordination_database(coordination_path: &FsPath) -> Result<rusqlite::Connection> {
    if let Some(parent) = coordination_path.parent() {
        create_local_state_directory(parent)?;
    }
    reject_existing_sqlite_state_path_redirection(coordination_path)?;
    let connection = open_sqlite_state_connection_unconfigured(coordination_path)?;
    ensure_sqlite_coordination_schema_for_path(&connection, coordination_path)?;
    {
        let write_lock = sqlite_state_write_lock(coordination_path);
        let _write_guard = lock_sqlite_state_writer(&write_lock);
        let setup = configure_sqlite_state_connection(&connection).with_context(|| {
            format!(
                "failed to configure SQLite pragmas for `{}`",
                coordination_path.display()
            )
        });
        finish_sqlite_state_file_setup(coordination_path, setup)?;
    }
    verify_persist_commit_integrity(coordination_path)?;
    Ok(connection)
}

#[cfg(test)]
mod sqlite_coordination_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn path_aware_coordination_setup_enforces_foreign_keys_before_fixture_writes() {
        let root = TestTempRoot::create("termal-coordination-foreign-keys");
        let path = root.path().join("coordination.sqlite");
        for _ in 0..2 {
            let connection = open_sqlite_state_connection_unconfigured(&path).unwrap();
            connection.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
            ensure_sqlite_coordination_schema_for_path(&connection, &path).unwrap();
            assert_eq!(
                connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i32>(0)).unwrap(),
                1
            );
            connection.execute_batch(
                "INSERT INTO mailboxes(id, participant_key, created_at, next_sequence)
                 VALUES('parent', 'participants', 'now', 1);
                 INSERT INTO mailbox_participants(mailbox_id, session_id, display_name, joined_at)
                 VALUES('parent', 'session', 'Participant', 'now');
                 DELETE FROM mailboxes WHERE id = 'parent';"
            ).unwrap();
            assert_eq!(
                connection.query_row("SELECT COUNT(*) FROM mailbox_participants", [], |row| row.get::<_, i64>(0)).unwrap(),
                0,
                "participant deletion must cascade on fresh and reopened handles"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn coordination_setup_hardens_owned_sidecars_even_when_schema_validation_fails() {
        use std::os::unix::fs::PermissionsExt;
        for fail_schema in [false, true] {
            let root = TestTempRoot::create("termal-coordination-permission-phases");
            let path = root.path().join("coordination.sqlite");
            let connection = bootstrap_coordination_database(&path).unwrap();
            if fail_schema {
                connection.execute("UPDATE meta SET value = 'wrong' WHERE key = 'coordination_schema_version'", []).unwrap();
            } else {
                connection.execute("UPDATE meta SET value = value", []).unwrap();
            }
            let sidecars = [sqlite_sidecar_path(&path, "-wal"), sqlite_sidecar_path(&path, "-shm")];
            for file in &sidecars {
                assert!(file.is_file());
                fs::set_permissions(file, fs::Permissions::from_mode(0o666)).unwrap();
            }
            let expected_error = ensure_sqlite_coordination_schema(&connection).err().map(|error| format!(
                "failed to open or validate coordination database `{}`: {error:#}", path.display()
            ));
            let result = ensure_sqlite_coordination_schema_for_path(&connection, &path);
            assert_eq!(result.err().map(|error| format!("{error:#}")), expected_error);
            for file in &sidecars {
                assert_eq!(fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600);
            }
        }
    }

    struct SchemaInitializationChild {
        child: Option<Child>,
    }

    impl SchemaInitializationChild {
        fn new(child: Child) -> Self {
            Self { child: Some(child) }
        }

        fn terminate_and_capture(&mut self) -> String {
            let Some(mut child) = self.child.take() else {
                return "child already reaped".to_owned();
            };
            let _ = child.kill();
            match child.wait_with_output() {
                Ok(output) => format!(
                    "status: {}\nstdout:\n{}\nstderr:\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
                Err(error) => format!("failed to collect child output: {error}"),
            }
        }

        fn wait_with_output(mut self, description: &str) -> std::process::Output {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let status = self
                    .child
                    .as_mut()
                    .expect("schema initialization child should still be owned")
                    .try_wait();
                match status {
                    Ok(Some(_)) => {
                        return self
                            .child
                            .take()
                            .expect("finished child should still be owned")
                            .wait_with_output()
                            .expect("schema initialization child output should collect");
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(None) => {
                        let report = self.terminate_and_capture();
                        panic!("timed out waiting for {description}\n{report}");
                    }
                    Err(error) => {
                        let report = self.terminate_and_capture();
                        panic!("failed to inspect {description}: {error}\n{report}");
                    }
                }
            }
        }
    }

    impl Drop for SchemaInitializationChild {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn expected_coordination_tables() -> BTreeSet<String> {
        expected_coordination_schema_objects()
            .expect("canonical coordination schema should build")
            .iter()
            .filter(|object| object.object_type == "table")
            .map(|object| object.name.clone())
            .collect()
    }

    fn wait_for_test_path(path: &FsPath, description: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}: {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_child_readiness(
        path: &FsPath,
        description: &str,
        children: &mut [SchemaInitializationChild],
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            if Instant::now() >= deadline {
                let reports = children
                    .iter_mut()
                    .enumerate()
                    .map(|(index, child)| {
                        format!("child {index}:\n{}", child.terminate_and_capture())
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                panic!(
                    "timed out waiting for {description}: {}\n{reports}",
                    path.display()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_reset_guidance(error: &anyhow::Error) {
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("unsupported coordination database schema"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Move or delete that coordination database"),
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
            coordination_schema_objects(&connection)
                .expect("schema inventory should read")
                .into_iter()
                .filter(|object| object.object_type == "table")
                .map(|object| object.name)
                .collect::<BTreeSet<_>>(),
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
        ensure_sqlite_coordination_schema(&connection).expect("current schema should initialize");
        connection
            .execute_batch("PRAGMA query_only = ON;")
            .expect("test connection should enter query-only mode");

        ensure_sqlite_coordination_schema(&connection)
            .expect("current schema checks must not require a write");
    }

    #[test]
    fn current_coordination_schema_initialization_is_safe_across_processes() {
        const TEST_NAME: &str =
            "current_coordination_schema_initialization_is_safe_across_processes";
        const DATABASE_ENV: &str = "TERMAL_TEST_COORDINATION_INIT_DATABASE";
        const READY_ENV: &str = "TERMAL_TEST_COORDINATION_INIT_READY";
        const GO_ENV: &str = "TERMAL_TEST_COORDINATION_INIT_GO";

        if let (Some(database), Some(ready), Some(go)) = (
            std::env::var_os(DATABASE_ENV),
            std::env::var_os(READY_ENV),
            std::env::var_os(GO_ENV),
        ) {
            let connection = open_sqlite_state_connection_unconfigured(FsPath::new(&database))
                .expect("child SQLite connection should open");
            fs::write(&ready, b"ready").expect("child readiness marker should write");
            wait_for_test_path(FsPath::new(&go), "schema initialization release marker");
            ensure_sqlite_coordination_schema_for_path(&connection, FsPath::new(&database))
                .expect("cross-process current-schema ensure should succeed");
            return;
        }

        let root = TestTempRoot::create("termal-current-coordination-schema");
        let path = root.path().join("coordination.sqlite");
        let go = root.path().join("go");
        let test_executable = std::env::current_exe().expect("test executable should resolve");
        let test_module = module_path!()
            .split_once("::")
            .map_or(module_path!(), |(_, module)| module);
        let exact_test_filter = format!("{test_module}::{TEST_NAME}");
        let mut children = Vec::new();
        let mut ready_paths = Vec::new();
        for index in 0..2 {
            let ready = root.path().join(format!("ready-{index}"));
            let child = Command::new(&test_executable)
                .arg("--exact")
                .arg(&exact_test_filter)
                .arg("--nocapture")
                .env(DATABASE_ENV, &path)
                .env(READY_ENV, &ready)
                .env(GO_ENV, &go)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("schema initialization child should spawn");
            children.push(SchemaInitializationChild::new(child));
            ready_paths.push(ready);
        }
        for ready in &ready_paths {
            wait_for_child_readiness(
                ready,
                "schema initialization child readiness marker",
                &mut children,
            );
        }
        fs::write(&go, b"go").expect("schema initialization release marker should write");
        for child in children {
            let output = child.wait_with_output("schema initialization child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "schema initialization child failed with {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                stdout,
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                stdout.contains("running 1 test"),
                "exact child filter `{exact_test_filter}` did not select one test\nstdout:\n{stdout}"
            );
        }

        let connection = rusqlite::Connection::open(&path).expect("schema database should reopen");
        validate_current_coordination_schema(&connection)
            .expect("concurrently initialized schema should be current");
        let metadata_count: u32 = connection
            .query_row("SELECT COUNT(*) FROM meta", [], |row| row.get(0))
            .expect("metadata count should read");
        assert_eq!(metadata_count, 1);
        drop(connection);
    }

    #[test]
    fn obsolete_coordination_schema_is_rejected_without_backfill() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_coordination_schema(&connection).expect("current schema should initialize");
        connection
            .execute_batch("ALTER TABLE mailbox_messages DROP COLUMN dispatch_outcome;")
            .expect("fixture should remove the current immutable outcome column");
        let before = coordination_schema_objects(&connection)
            .expect("obsolete schema inventory should read");

        let error = ensure_sqlite_coordination_schema(&connection)
            .expect_err("obsolete coordination schema must be rejected");
        assert_reset_guidance(&error);
        assert_eq!(
            coordination_schema_objects(&connection)
                .expect("rejected schema inventory should remain readable"),
            before,
            "schema validation must not mutate or backfill obsolete state"
        );
    }

    #[test]
    fn same_column_names_with_noncanonical_physical_schema_are_rejected() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_coordination_schema(&connection).expect("current schema should initialize");
        connection
            .execute_batch(
                "
                DROP INDEX mailbox_participants_session;
                DROP TABLE mailbox_participants;
                CREATE TABLE mailbox_participants (
                  mailbox_id TEXT NOT NULL,
                  session_id INTEGER NOT NULL,
                  display_name TEXT NOT NULL,
                  processed_through INTEGER DEFAULT 0,
                  joined_at TEXT NOT NULL,
                  left_at TEXT,
                  PRIMARY KEY (mailbox_id, session_id)
                );
                CREATE INDEX mailbox_participants_session
                  ON mailbox_participants(session_id, left_at);
                ",
            )
            .expect("fixture should install matching names with changed physical constraints");
        let before = coordination_schema_objects(&connection)
            .expect("noncanonical schema inventory should read");
        connection
            .execute_batch("PRAGMA query_only = ON;")
            .expect("test connection should enter query-only mode");

        let error = ensure_sqlite_coordination_schema(&connection)
            .expect_err("same column names must not hide a noncanonical physical schema");
        assert_reset_guidance(&error);
        assert_eq!(
            coordination_schema_objects(&connection)
                .expect("rejected schema inventory should remain readable"),
            before,
            "physical-schema validation must remain read-only"
        );
    }

    #[test]
    fn noncanonical_named_index_definition_is_rejected() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_coordination_schema(&connection).expect("current schema should initialize");
        connection
            .execute_batch(
                "
                DROP INDEX mailbox_participants_session;
                CREATE INDEX mailbox_participants_session
                  ON mailbox_participants(left_at, session_id);
                PRAGMA query_only = ON;
                ",
            )
            .expect("fixture should install a noncanonical named index");

        let error = ensure_sqlite_coordination_schema(&connection)
            .expect_err("changed named index definition must be rejected");
        assert_reset_guidance(&error);
    }

    #[test]
    fn partial_mailbox_only_schema_is_rejected_without_mutation() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        connection
            .execute_batch(
                "
                CREATE TABLE mailboxes (
                  id TEXT PRIMARY KEY,
                  participant_key TEXT NOT NULL UNIQUE,
                  created_at TEXT NOT NULL,
                  next_sequence INTEGER NOT NULL
                );
                INSERT INTO mailboxes(id, participant_key, created_at, next_sequence)
                VALUES('partial-mailbox', 'a\\nb', '2026-09-03T00:00:00Z', 1);
                ",
            )
            .expect("partial mailbox fixture should initialize");
        let before =
            coordination_schema_objects(&connection).expect("partial schema inventory should read");
        connection
            .execute_batch("PRAGMA query_only = ON;")
            .expect("test connection should enter query-only mode");

        let error = ensure_sqlite_coordination_schema(&connection)
            .expect_err("partial mailbox-only schema must be rejected");
        assert_reset_guidance(&error);
        assert_eq!(
            coordination_schema_objects(&connection)
                .expect("partial schema inventory should remain readable"),
            before,
            "partial schema rejection must not create missing objects"
        );
        let mailbox_count: u32 = connection
            .query_row("SELECT COUNT(*) FROM mailboxes", [], |row| row.get(0))
            .expect("partial mailbox row should remain readable");
        assert_eq!(mailbox_count, 1);
    }

    #[test]
    fn unsupported_coordination_schema_version_is_rejected_without_rewrite() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_coordination_schema(&connection).expect("current schema should initialize");
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
    fn missing_coordination_schema_version_is_rejected_without_rewrite() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_coordination_schema(&connection).expect("current schema should initialize");
        connection
            .execute(
                "DELETE FROM meta WHERE key = 'coordination_schema_version'",
                [],
            )
            .expect("fixture should remove the schema version");
        connection
            .execute_batch("PRAGMA query_only = ON;")
            .expect("test connection should enter query-only mode");

        let error = ensure_sqlite_coordination_schema(&connection)
            .expect_err("missing schema version must be rejected");
        assert_reset_guidance(&error);
        let metadata_count: u32 = connection
            .query_row("SELECT COUNT(*) FROM meta", [], |row| row.get(0))
            .expect("metadata count should remain readable");
        assert_eq!(metadata_count, 0);
    }

    #[test]
    fn unreadable_coordination_schema_version_is_an_operational_error() {
        let root = TestTempRoot::create("termal-unreadable-coordination-version");
        let path = root.path().join("coordination.sqlite");
        let connection =
            rusqlite::Connection::open(&path).expect("coordination database should open");
        ensure_sqlite_coordination_schema(&connection).expect("current schema should initialize");
        connection
            .execute(
                "UPDATE meta SET value = x'00' WHERE key = 'coordination_schema_version'",
                [],
            )
            .expect("fixture should install an unreadable version value");

        let error = ensure_sqlite_coordination_schema_for_path(&connection, &path)
            .expect_err("unreadable schema version must remain an operational error");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("failed to open or validate coordination database"),
            "{rendered}"
        );
        assert!(rendered.contains(&path.display().to_string()), "{rendered}");
        assert!(
            rendered.contains("failed to read the coordination schema version"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("Move or delete `coordination.sqlite`"),
            "operational read errors must not be mislabeled as resettable schema drift: {rendered}"
        );
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

    #[test]
    fn coordination_bootstrap_schema_error_names_the_actual_database_path() {
        let root = std::env::temp_dir().join(format!(
            "termal-current-coordination-path-error-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("test directory should exist");
        let path = root.join("custom-coordination.sqlite");
        let connection = rusqlite::Connection::open(&path).expect("schema database should open");
        connection
            .execute_batch("CREATE TABLE obsolete(value TEXT);")
            .expect("obsolete schema should initialize");
        drop(connection);

        let error = bootstrap_coordination_database(&path)
            .expect_err("obsolete coordination schema must fail bootstrap");
        let rendered = format!("{error:#}");
        assert_reset_guidance(&error);
        assert!(rendered.contains(&path.display().to_string()), "{rendered}");
        assert!(
            !rendered.contains("Move or delete `coordination.sqlite`"),
            "reset guidance must not name a different database: {rendered}"
        );

        fs::remove_dir_all(root).expect("test directory should clean up");
    }
}
