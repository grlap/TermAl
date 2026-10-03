// Split out of persist.rs: owns the current state schema inventory, validation,
// initialization, metadata-authority checks and shared schema fixture seeds.
// Does not own connection lifetimes, path locks, transaction orchestration,
// physical message layout conversion, overview backfill or transcript reads.

fn sqlite_state_user_table_names(connection: &rusqlite::Connection) -> Result<BTreeSet<String>> {
    let mut statement = connection
        .prepare(
            "SELECT name
             FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .context("failed to inspect state database tables")?;
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .context("failed to query state database tables")?
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .context("failed to decode state database tables")
}

fn sqlite_state_table_columns(
    connection: &rusqlite::Connection,
    table_name: &str,
) -> Result<BTreeSet<String>> {
    // Callers only pass names from CURRENT_SQLITE_STATE_TABLE_COLUMNS. SQLite
    // PRAGMA table_info does not accept a bound table-name parameter.
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table_name})"))
        .with_context(|| format!("failed to inspect state table `{table_name}`"))?;
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .with_context(|| format!("failed to query state table `{table_name}`"))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .with_context(|| format!("failed to decode state table `{table_name}`"))
}

const CURRENT_SQLITE_STATE_TABLE_COLUMNS: &[(&str, &[&str])] = &[
    ("meta", &["key", "value"]),
    ("sessions", &["id", "value_json"]),
    (
        "messages",
        &[
            "session_id",
            "position",
            "message_id",
            "value_json",
            "overview_kind",
            "is_user",
        ],
    ),
    ("session_overviews", &["session_id", "value_blob"]),
    ("app_state", &["key", "value_json"]),
    ("session_prompt_histories", &["session_id", "value_json"]),
    ("delegations", &["id", "value_json"]),
    (
        "response_board_tabs",
        &[
            "id",
            "name",
            "kind",
            "project_id",
            "sort_order",
            "created_at",
        ],
    ),
    (
        "board_cards",
        &[
            "id",
            "x",
            "y",
            "w",
            "h",
            "snapshot_json",
            "source_session_id",
            "source_message_id",
            "created_at",
            "tab_id",
            "placement",
            "has_canvas_position",
        ],
    ),
];

// These normalized tables contain durable state that a fresh in-memory
// bootstrap must never adopt without the matching global metadata authority.
// `response_board_tabs` is intentionally absent: schema initialization seeds
// the deterministic default tab before the first metadata snapshot is written.
const SQLITE_STATE_METADATA_AUTHORITY_TABLES: &[&str] = &[
    "sessions",
    "messages",
    "session_overviews",
    "session_prompt_histories",
    "delegations",
    "board_cards",
];

fn sqlite_state_column_may_be_added_by_maintenance(table_name: &str, column_name: &str) -> bool {
    matches!(
        (table_name, column_name),
        ("messages", "overview_kind" | "is_user")
    )
}

fn validate_sqlite_state_table_columns(
    connection: &rusqlite::Connection,
    expected_tables: &[(&str, &[&str])],
    allow_maintenance_missing: bool,
) -> Result<()> {
    for (table_name, expected_columns) in expected_tables {
        let actual_columns = sqlite_state_table_columns(connection, table_name)?;
        let expected_columns = expected_columns
            .iter()
            .map(|column_name| (*column_name).to_owned())
            .collect::<BTreeSet<_>>();
        let unexpected_columns = actual_columns
            .difference(&expected_columns)
            .cloned()
            .collect::<Vec<_>>();
        let missing_columns = expected_columns
            .difference(&actual_columns)
            .filter(|column_name| {
                !allow_maintenance_missing
                    || !sqlite_state_column_may_be_added_by_maintenance(table_name, column_name)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !unexpected_columns.is_empty() || !missing_columns.is_empty() {
            let mut differences = Vec::new();
            if !missing_columns.is_empty() {
                differences.push(format!("missing columns {missing_columns:?}"));
            }
            if !unexpected_columns.is_empty() {
                differences.push(format!("unexpected columns {unexpected_columns:?}"));
            }
            return Err(reject_unsupported_state_schema(format!(
                "table `{table_name}` has {}; expected columns {expected_columns:?}, found \
                 {actual_columns:?}",
                differences.join(" and ")
            )));
        }
    }
    Ok(())
}

const SQLITE_STATE_CORE_SCHEMA_SQL: &str = "
    CREATE TABLE meta (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL
    );

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
    );

    CREATE TABLE session_overviews (
      session_id TEXT PRIMARY KEY,
      value_blob BLOB NOT NULL,
      FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
    ) WITHOUT ROWID;
";

const SQLITE_STATE_AUXILIARY_SCHEMA_SQL: &str = "
    CREATE TABLE app_state (
      key TEXT PRIMARY KEY,
      value_json TEXT NOT NULL
    );

    CREATE TABLE session_prompt_histories (
      session_id TEXT PRIMARY KEY,
      value_json TEXT NOT NULL,
      FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
    ) WITHOUT ROWID;

    CREATE TABLE delegations (
      id TEXT PRIMARY KEY,
      value_json TEXT NOT NULL
    );

    CREATE TABLE response_board_tabs (
      id TEXT PRIMARY KEY,
      name TEXT NOT NULL,
      kind TEXT NOT NULL,
      project_id TEXT UNIQUE,
      sort_order INTEGER NOT NULL,
      created_at TEXT NOT NULL
    );

    CREATE TABLE board_cards (
      id TEXT PRIMARY KEY,
      x REAL NOT NULL,
      y REAL NOT NULL,
      w REAL NOT NULL,
      h REAL NOT NULL,
      snapshot_json TEXT NOT NULL,
      source_session_id TEXT NOT NULL,
      source_message_id TEXT NOT NULL,
      created_at TEXT NOT NULL,
      tab_id TEXT NOT NULL DEFAULT 'response-board-default',
      placement TEXT NOT NULL DEFAULT 'placed',
      has_canvas_position INTEGER NOT NULL DEFAULT 1
    );
";

fn reject_unsupported_state_schema(detail: impl std::fmt::Display) -> anyhow::Error {
    anyhow!(
        "unsupported state database schema ({detail}); this unreleased local state is not migrated. \
         Move or delete `termal.sqlite` to reset local app state, then restart TermAl"
    )
}

fn required_state_meta_value(connection: &rusqlite::Connection, key: &str) -> Result<String> {
    connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .with_context(|| format!("failed to read state metadata key `{key}`"))?
        .ok_or_else(|| reject_unsupported_state_schema(format!("missing `{key}` metadata")))
}

fn embedded_state_field_has_records_or_invalid_shape(value: &Value, key: &str) -> bool {
    match value.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::Array(entries)) => !entries.is_empty(),
        Some(_) => true,
    }
}

fn first_normalized_state_authority_table_with_rows(
    connection: &rusqlite::Connection,
) -> Result<Option<&'static str>> {
    for table_name in SQLITE_STATE_METADATA_AUTHORITY_TABLES {
        // Table names come only from the static inventory above. SQLite does
        // not allow a bound identifier in this EXISTS query.
        let has_rows = connection
            .query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table_name} LIMIT 1)"),
                [],
                |row| row.get::<_, bool>(0),
            )
            .with_context(|| format!("failed to inspect normalized `{table_name}` authority"))?;
        if has_rows {
            return Ok(Some(table_name));
        }
    }
    Ok(None)
}

#[cfg(test)]
thread_local! {
    static LEGACY_EMBEDDED_SESSION_SCAN_COUNT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static PERSISTED_STATE_METADATA_DESERIALIZE_COUNT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_legacy_embedded_session_scan_count() {
    LEGACY_EMBEDDED_SESSION_SCAN_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn legacy_embedded_session_scan_count() -> usize {
    LEGACY_EMBEDDED_SESSION_SCAN_COUNT.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_persisted_state_metadata_deserialize_count() {
    PERSISTED_STATE_METADATA_DESERIALIZE_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn persisted_state_metadata_deserialize_count() -> usize {
    PERSISTED_STATE_METADATA_DESERIALIZE_COUNT.with(std::cell::Cell::get)
}

fn reject_invalid_persisted_state_metadata_shape(error: serde_json::Error) -> anyhow::Error {
    let mut detail = error.to_string();
    let location = format!(" at line {} column {}", error.line(), error.column());
    if detail.ends_with(&location) {
        detail.truncate(detail.len() - location.len());
    }
    reject_unsupported_state_schema(format!(
        "persisted state metadata does not match the current metadata shape: {detail}"
    ))
}

fn validate_no_legacy_embedded_state(
    connection: &rusqlite::Connection,
) -> Result<Option<PersistedState>> {
    let metadata = connection
        .query_row(
            "SELECT value_json FROM app_state WHERE key = ?1",
            rusqlite::params![SQLITE_METADATA_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to inspect state metadata authority")?;
    let persisted = if let Some(encoded) = metadata {
        let value: Value = serde_json::from_str(&encoded).map_err(|error| {
            reject_unsupported_state_schema(format!(
                "persisted state metadata is not valid JSON: {error}"
            ))
        })?;
        for key in ["sessions", "delegations"] {
            if embedded_state_field_has_records_or_invalid_shape(&value, key) {
                return Err(reject_unsupported_state_schema(format!(
                    "app_state contains embedded `{key}` records or an invalid `{key}` shape"
                )));
            }
        }
        #[cfg(test)]
        PERSISTED_STATE_METADATA_DESERIALIZE_COUNT.with(|count| count.set(count.get() + 1));
        let persisted = serde_json::from_str::<PersistedState>(&encoded)
            .map_err(reject_invalid_persisted_state_metadata_shape)?;
        Some(persisted)
    } else if let Some(table_name) = first_normalized_state_authority_table_with_rows(connection)? {
        return Err(reject_unsupported_state_schema(format!(
            "missing app_state `{SQLITE_METADATA_KEY}` metadata while normalized table \
             `{table_name}` contains rows"
        )));
    } else {
        None
    };

    #[cfg(test)]
    LEGACY_EMBEDDED_SESSION_SCAN_COUNT.with(|count| count.set(count.get() + 1));
    let embedded_prompt_history_session = connection
        .query_row(
            "SELECT id
             FROM sessions
             WHERE typeof(id) = 'text'
               AND typeof(value_json) = 'text'
               AND CASE
                     WHEN json_valid(value_json)
                     THEN json_type(value_json, '$.session.promptHistory')
                   END = 'array'
             LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("failed to inspect embedded prompt-history authority")?;
    if let Some(session_id) = embedded_prompt_history_session {
        return Err(reject_unsupported_state_schema(format!(
            "session `{session_id}` still contains embedded `session.promptHistory`"
        )));
    }
    Ok(persisted)
}

fn validate_current_sqlite_state_schema(connection: &rusqlite::Connection) -> Result<()> {
    let actual_tables = sqlite_state_user_table_names(connection)?;
    if !actual_tables.contains("meta") {
        return Err(reject_unsupported_state_schema(format!(
            "missing `meta` table; found tables {actual_tables:?}"
        )));
    }
    let meta_table = CURRENT_SQLITE_STATE_TABLE_COLUMNS
        .iter()
        .find(|(table_name, _)| *table_name == "meta")
        .ok_or_else(|| anyhow!("current state schema inventory is missing the `meta` table"))?;
    validate_sqlite_state_table_columns(connection, std::slice::from_ref(meta_table), false)?;
    let stored_schema_version = required_state_meta_value(connection, "schema_version")?;
    if stored_schema_version != SQLITE_SCHEMA_VERSION {
        return Err(reject_unsupported_state_schema(format!(
            "found version `{stored_schema_version}`, expected `{SQLITE_SCHEMA_VERSION}`"
        )));
    }

    let missing_tables = CURRENT_SQLITE_STATE_TABLE_COLUMNS
        .iter()
        .map(|(table_name, _)| *table_name)
        .filter(|table_name| !actual_tables.contains(*table_name))
        .collect::<Vec<_>>();
    if !missing_tables.is_empty() {
        return Err(reject_unsupported_state_schema(format!(
            "missing required tables {missing_tables:?}; found tables {actual_tables:?}"
        )));
    }
    validate_sqlite_state_table_columns(connection, CURRENT_SQLITE_STATE_TABLE_COLUMNS, true)?;
    let prompt_history_storage_version =
        required_state_meta_value(connection, SQLITE_PROMPT_HISTORY_STORAGE_KEY)?;
    if prompt_history_storage_version != SQLITE_PROMPT_HISTORY_STORAGE_VERSION {
        return Err(reject_unsupported_state_schema(format!(
            "found `{SQLITE_PROMPT_HISTORY_STORAGE_KEY}` value \
             `{prompt_history_storage_version}`, expected `{SQLITE_PROMPT_HISTORY_STORAGE_VERSION}`"
        )));
    }
    Ok(())
}

fn initialize_current_sqlite_state_schema(connection: &rusqlite::Connection) -> Result<bool> {
    let transaction =
        rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)
            .context("failed to begin current state schema initialization")?;
    if !sqlite_state_user_table_names(&transaction)?.is_empty() {
        transaction
            .commit()
            .context("failed to finish concurrent state schema initialization check")?;
        return Ok(false);
    }
    transaction
        .execute_batch(SQLITE_STATE_CORE_SCHEMA_SQL)
        .context("failed to initialize current SQLite core state schema")?;
    transaction
        .execute_batch(SQLITE_STATE_AUXILIARY_SCHEMA_SQL)
        .context("failed to initialize current SQLite auxiliary state schema")?;
    ensure_sqlite_message_overview_columns(&transaction)?;
    ensure_sqlite_response_board_schema(&transaction)?;
    backfill_missing_sqlite_session_overviews(&transaction)?;
    validate_sqlite_state_table_columns(&transaction, CURRENT_SQLITE_STATE_TABLE_COLUMNS, false)?;
    transaction
        .execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)",
            rusqlite::params![SQLITE_SCHEMA_VERSION],
        )
        .context("failed to record SQLite state schema version")?;
    transaction
        .execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)",
            rusqlite::params![
                SQLITE_PROMPT_HISTORY_STORAGE_KEY,
                SQLITE_PROMPT_HISTORY_STORAGE_VERSION
            ],
        )
        .context("failed to record SQLite prompt-history storage authority")?;
    transaction
        .commit()
        .context("failed to commit current state schema initialization")?;
    Ok(true)
}

/// Performs the bounded schema setup required by write-only connections.
///
/// Deep metadata deserialization and the legacy session-JSON authority scan
/// belong exclusively to [`ensure_sqlite_state_schema_for_load`].
fn ensure_sqlite_state_schema(connection: &rusqlite::Connection) -> Result<()> {
    if sqlite_state_user_table_names(connection)?.is_empty()
        && initialize_current_sqlite_state_schema(connection)?
    {
        configure_sqlite_state_connection(connection)?;
        return Ok(());
    }

    // Existing databases must pass every read-only compatibility check before
    // a persistent PRAGMA, CREATE/ALTER, backfill, or metadata write can run.
    validate_current_sqlite_state_schema(connection)?;
    finish_existing_sqlite_state_schema_setup(connection)
}

/// Applies maintenance after the caller's applicable read-only checks.
///
/// The startup load path captures and returns the persisted `app_state`
/// metadata before calling this helper. Consequently, maintenance here must
/// not mutate `app_state`; a future metadata migration must instead move the
/// capture to after that migration so the loader cannot observe stale state.
fn finish_existing_sqlite_state_schema_setup(connection: &rusqlite::Connection) -> Result<()> {
    configure_sqlite_state_connection(connection)?;
    ensure_sqlite_message_overview_columns(connection)?;
    ensure_sqlite_message_rowid_storage(connection)?;
    ensure_sqlite_response_board_schema(connection)?;
    backfill_missing_sqlite_session_overviews(connection)?;
    validate_sqlite_state_table_columns(connection, CURRENT_SQLITE_STATE_TABLE_COLUMNS, false)?;
    Ok(())
}

/// Prepares the SQLite state database for a startup load and returns the
/// already-deserialized metadata row.
///
/// Startup alone pays for the fail-closed legacy-authority checks. Returning
/// the value parsed from the raw metadata string keeps the guard and loader on
/// one `serde_json::from_str` path instead of re-reading and re-parsing it.
fn ensure_sqlite_state_schema_for_load(
    connection: &rusqlite::Connection,
) -> Result<Option<PersistedState>> {
    if sqlite_state_user_table_names(connection)?.is_empty()
        && initialize_current_sqlite_state_schema(connection)?
    {
        configure_sqlite_state_connection(connection)?;
        return Ok(None);
    }

    // Existing databases must pass every read-only compatibility and startup
    // authority check before a persistent PRAGMA or maintenance write can run.
    validate_current_sqlite_state_schema(connection)?;
    let persisted = validate_no_legacy_embedded_state(connection)?;
    finish_existing_sqlite_state_schema_setup(connection)?;
    Ok(persisted)
}

#[cfg(test)]
fn seed_current_state_auxiliary_tables(connection: &rusqlite::Connection) {
    connection
        .execute_batch(SQLITE_STATE_AUXILIARY_SCHEMA_SQL)
        .expect("current auxiliary state tables should initialize");
}

/// Shared current core fixture. Tests may explicitly drop only the two
/// maintenance-owned message columns to exercise validate-before-backfill.
#[cfg(test)]
fn seed_current_state_core_tables(connection: &rusqlite::Connection) {
    connection
        .execute_batch(SQLITE_STATE_CORE_SCHEMA_SQL)
        .expect("current core state tables should initialize");
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .expect("fixture writes must enforce foreign keys");
    for (key, value) in [
        ("schema_version", SQLITE_SCHEMA_VERSION),
        (
            SQLITE_PROMPT_HISTORY_STORAGE_KEY,
            SQLITE_PROMPT_HISTORY_STORAGE_VERSION,
        ),
    ] {
        connection
            .execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2)",
                rusqlite::params![key, value],
            )
            .expect("current authority marker should seed");
    }
}

#[cfg(test)]
fn seed_current_state_metadata(connection: &rusqlite::Connection) {
    connection
        .execute(
            "INSERT INTO app_state(key, value_json) VALUES(?1, ?2)",
            rusqlite::params![
                SQLITE_METADATA_KEY,
                r#"{"nextSessionNumber":1,"nextMessageNumber":1,"projects":[],"sessions":[]}"#
            ],
        )
        .expect("current metadata authority should seed");
}
