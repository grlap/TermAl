/*
SQLite-backed session state persistence.

Owns the on-disk schema (`ensure_sqlite_state_schema`), connection lifecycle
(`open_sqlite_state_connection`, `SqlitePersistConnectionCache`), load path
(`load_state`, `load_state_from_sqlite`, `read_json_persisted_state`), and
the per-transaction write helpers used by the background persist thread
(`persist_state_parts_via_connection`, `persist_delta_via_cache`,
`persist_created_session`, `persist_state_from_persisted`, `persist_state`).

Extracted from `api.rs` so HTTP handler code and SQLite persistence live
in separate files. The crate still compiles as one `include!()`-assembled
module, so no visibility changes are required.
*/

/// Resolves persistence path.
fn resolve_persistence_path(default_workdir: &str) -> PathBuf {
    resolve_termal_data_dir(default_workdir).join("sessions.json")
}

#[cfg(not(test))]
const SQLITE_SCHEMA_VERSION: &str = "1";
#[cfg(not(test))]
const SQLITE_LEGACY_STATE_KEY: &str = "persistedState";
#[cfg(not(test))]
const SQLITE_METADATA_KEY: &str = "metadataState";
#[cfg(not(test))]
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(not(test))]
fn sqlite_persistence_path_for_json_path(path: &FsPath) -> PathBuf {
    path.with_file_name("termal.sqlite")
}

#[cfg(not(test))]
fn open_sqlite_state_connection(path: &FsPath) -> Result<rusqlite::Connection> {
    let connection = rusqlite::Connection::open(path)
        .with_context(|| format!("failed to open `{}`", path.display()))?;
    connection
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .with_context(|| format!("failed to set SQLite busy timeout for `{}`", path.display()))?;
    // WAL lets readers coexist with the background persistence writer. NORMAL
    // sync is the common local-app tradeoff: durable enough for TermAl state,
    // with much lower fsync cost than FULL on every small create-session write.
    connection
        .execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            ",
        )
        .with_context(|| format!("failed to configure SQLite pragmas for `{}`", path.display()))?;
    Ok(connection)
}

fn read_json_persisted_state(path: &FsPath) -> Result<PersistedState> {
    let raw = fs::read(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    let encoded: Value = serde_json::from_slice(&raw)
        .with_context(|| format!("failed to parse `{}`", path.display()))?;
    serde_json::from_value(encoded)
        .with_context(|| format!("failed to deserialize state from `{}`", path.display()))
}

/// Loads state.
#[cfg(test)]
fn load_state(path: &FsPath) -> Result<Option<StateInner>> {
    if !path.exists() {
        return Ok(None);
    }

    let persisted = read_json_persisted_state(path)?;
    Ok(Some(persisted.into_inner().with_context(|| {
        format!("failed to validate state from `{}`", path.display())
    })?))
}

/// Loads state from SQLite in production, importing the legacy JSON file once.
#[cfg(not(test))]
fn load_state(path: &FsPath) -> Result<Option<StateInner>> {
    let sqlite_path = sqlite_persistence_path_for_json_path(path);
    if sqlite_path.exists() {
        return load_state_from_sqlite(&sqlite_path);
    }
    if !path.exists() {
        return Ok(None);
    }

    let persisted = read_json_persisted_state(path)?;
    let inner = persisted.clone().into_inner().with_context(|| {
        format!("failed to validate state from `{}`", path.display())
    })?;
    persist_persisted_state_to_sqlite(&sqlite_path, &persisted)?;

    let backup_path = imported_json_backup_path(path)?;
    if let Err(err) = fs::rename(path, &backup_path) {
        if let Err(cleanup_err) = fs::remove_file(&sqlite_path) {
            eprintln!(
                "[termal] failed to remove incomplete SQLite import `{}` after rename failure: {cleanup_err}",
                sqlite_path.display()
            );
        }
        return Err(err).with_context(|| {
            format!(
                "failed to rename imported state `{}` to `{}`",
                path.display(),
                backup_path.display()
            )
        });
    }

    eprintln!(
        "[termal] imported `{}` into `{}`; legacy backup renamed to `{}`",
        path.display(),
        sqlite_path.display(),
        backup_path.display()
    );
    Ok(Some(inner))
}

#[cfg(not(test))]
fn imported_json_backup_path(path: &FsPath) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| FsPath::new(""));
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("sessions");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("json");
    let timestamp = Local::now().format("%Y-%m-%d-%H%M%S");

    for attempt in 0..1000 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let candidate = parent.join(format!(
            "{stem}.imported-{timestamp}{suffix}.{extension}"
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(anyhow!(
        "failed to choose an unused imported backup path for `{}`",
        path.display()
    ))
}

#[cfg(not(test))]
fn ensure_sqlite_state_schema(connection: &rusqlite::Connection) -> Result<()> {
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS meta (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS app_state (
              key TEXT PRIMARY KEY,
              value_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
              id TEXT PRIMARY KEY,
              value_json TEXT NOT NULL
            );
            ",
        )
        .context("failed to initialize SQLite state schema")?;
    connection
        .execute(
            "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![SQLITE_SCHEMA_VERSION],
        )
        .context("failed to record SQLite state schema version")?;
    Ok(())
}

#[cfg(not(test))]
fn load_state_from_sqlite(path: &FsPath) -> Result<Option<StateInner>> {
    let connection = open_sqlite_state_connection(path)?;
    ensure_sqlite_state_schema(&connection)?;
    let session_records = load_session_records_from_sqlite(&connection, path)?;
    // The legacy lookup is only consulted when the primary key is
    // missing. `.or(...)` would eagerly run both queries (both sides
    // of `.or` must be evaluated before the combinator sees them),
    // which pays for a second `SELECT ... FROM app_state WHERE key = ?`
    // round-trip against the connection on every startup — silent
    // but wasteful on the happy path where `SQLITE_METADATA_KEY` is
    // always present post-migration. Structure as `if let` chains so
    // the legacy query only runs when the primary returns `None`.
    let encoded = if let Some(encoded) =
        sqlite_app_state_value(&connection, SQLITE_METADATA_KEY, path)?
    {
        encoded
    } else if let Some(encoded) =
        sqlite_app_state_value(&connection, SQLITE_LEGACY_STATE_KEY, path)?
    {
        encoded
    } else {
        return Ok(None);
    };
    let mut persisted: PersistedState = serde_json::from_str(&encoded)
        .with_context(|| format!("failed to parse persisted state from `{}`", path.display()))?;
    if !session_records.is_empty() {
        persisted.sessions = session_records;
    }
    Ok(Some(persisted.into_inner().with_context(|| {
        format!("failed to validate state from `{}`", path.display())
    })?))
}

#[cfg(not(test))]
fn sqlite_app_state_value(
    connection: &rusqlite::Connection,
    key: &str,
    path: &FsPath,
) -> Result<Option<String>> {
    match connection.query_row(
        "SELECT value_json FROM app_state WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get::<_, String>(0),
    ) {
        Ok(encoded) => Ok(Some(encoded)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err)
            .with_context(|| format!("failed to read persisted state from `{}`", path.display())),
    }
}

#[cfg(not(test))]
fn load_session_records_from_sqlite(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<Vec<PersistedSessionRecord>> {
    let mut statement = connection
        .prepare("SELECT value_json FROM sessions ORDER BY rowid")
        .with_context(|| format!("failed to prepare session load from `{}`", path.display()))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .with_context(|| format!("failed to query persisted sessions from `{}`", path.display()))?;
    let mut records = Vec::new();
    for row in rows {
        let encoded =
            row.with_context(|| format!("failed to read session row from `{}`", path.display()))?;
        let record = serde_json::from_str(&encoded).with_context(|| {
            format!(
                "failed to parse persisted session row from `{}`",
                path.display()
            )
        })?;
        records.push(record);
    }
    Ok(records)
}

#[cfg(not(test))]
fn persist_persisted_state_to_sqlite(path: &FsPath, persisted: &PersistedState) -> Result<()> {
    let metadata = persisted.metadata_only();
    persist_state_parts_to_sqlite(path, &metadata, &persisted.sessions, true)
}

#[cfg(not(test))]
fn persist_created_session(path: &FsPath, inner: &StateInner, record: &SessionRecord) -> Result<()> {
    let metadata = PersistedState::metadata_from_inner(inner);
    let session = PersistedSessionRecord::from_record(record);
    persist_state_parts_to_sqlite(
        &sqlite_persistence_path_for_json_path(path),
        &metadata,
        std::slice::from_ref(&session),
        false,
    )
}

#[cfg(test)]
fn persist_created_session(path: &FsPath, inner: &StateInner, _record: &SessionRecord) -> Result<()> {
    let persisted = PersistedState::from_inner(inner);
    persist_state_from_persisted(path, &persisted)
}

#[cfg(not(test))]
fn persist_state_parts_to_sqlite(
    path: &FsPath,
    metadata: &PersistedState,
    sessions: &[PersistedSessionRecord],
    replace_sessions: bool,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let mut connection = open_sqlite_state_connection(path)?;
    ensure_sqlite_state_schema(&connection)?;
    persist_state_parts_via_connection(&mut connection, path, metadata, sessions, replace_sessions)
}

/// Applies one persist transaction to an already-open SQLite connection.
///
/// Assumes the caller has run [`ensure_sqlite_state_schema`] at least once
/// for this connection. Used by the background persist thread so the
/// per-persist hot path does not pay for opening a fresh connection or
/// re-running the schema-version upsert on every commit.
#[cfg(not(test))]
fn persist_state_parts_via_connection(
    connection: &mut rusqlite::Connection,
    path: &FsPath,
    metadata: &PersistedState,
    sessions: &[PersistedSessionRecord],
    replace_sessions: bool,
) -> Result<()> {
    let metadata_json =
        serde_json::to_string(metadata).context("failed to serialize persisted state metadata")?;
    let tx = connection
        .transaction()
        .with_context(|| format!("failed to start SQLite transaction for `{}`", path.display()))?;
    tx.execute(
        "INSERT INTO app_state(key, value_json) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        rusqlite::params![SQLITE_METADATA_KEY, metadata_json],
    )
    .with_context(|| format!("failed to write state metadata to `{}`", path.display()))?;
    if replace_sessions {
        tx.execute("DELETE FROM sessions", [])
            .with_context(|| format!("failed to replace sessions in `{}`", path.display()))?;
    }
    for session in sessions {
        let session_json =
            serde_json::to_string(session).context("failed to serialize persisted session")?;
        tx.execute(
            "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET value_json = excluded.value_json",
            rusqlite::params![&session.session.id, session_json],
        )
        .with_context(|| format!("failed to write persisted session to `{}`", path.display()))?;
    }
    tx.commit()
        .with_context(|| format!("failed to commit persisted state to `{}`", path.display()))?;
    Ok(())
}

/// Thread-local SQLite connection cache for the background persist thread.
///
/// Every queued persist previously opened a fresh SQLite connection and
/// re-ran `ensure_sqlite_state_schema`, which writes `schema_version`
/// every call. The persist thread writes many times during an active
/// session, so amortizing that fixed cost to one open-and-validate per
/// thread lifetime removes the biggest per-persist overhead.
#[cfg(not(test))]
struct SqlitePersistConnectionCache {
    path: Option<PathBuf>,
    connection: Option<rusqlite::Connection>,
}

#[cfg(not(test))]
impl SqlitePersistConnectionCache {
    fn new() -> Self {
        Self {
            path: None,
            connection: None,
        }
    }

    /// Returns a mutable reference to a SQLite connection opened for
    /// `path`, reusing the cached connection when the path matches.
    /// Runs schema validation only when a fresh connection is opened.
    fn connection_for(&mut self, path: &FsPath) -> Result<&mut rusqlite::Connection> {
        let matches_cache = self.path.as_deref() == Some(path);
        if !matches_cache {
            // Path changed (or first open): drop any stale connection and
            // open+validate fresh. Dropping the stale connection first
            // lets rusqlite flush any pending state before we rebind.
            self.connection = None;
            self.path = None;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create `{}`", parent.display()))?;
            }
            let connection = open_sqlite_state_connection(path)?;
            ensure_sqlite_state_schema(&connection)?;
            self.path = Some(path.to_path_buf());
            self.connection = Some(connection);
        }
        Ok(self
            .connection
            .as_mut()
            .expect("connection was just cached for the requested path"))
    }
}

/// Applies a `PersistDelta` — metadata upsert plus targeted session
/// row `INSERT OR UPDATE`s and `DELETE`s — via the shared connection
/// cache.
///
/// This is the sole production write path. It writes only the rows in
/// `delta.changed_sessions` and removes only `delta.removed_session_ids`;
/// unchanged session rows are left untouched so a mutation on one
/// session no longer rewrites every other session row every commit.
/// See `state.rs::PersistDelta` and `StateInner::collect_persist_delta`
/// for the authoritative description of how the delta is assembled.
#[cfg(not(test))]
fn persist_delta_via_cache(
    cache: &mut SqlitePersistConnectionCache,
    path: &FsPath,
    delta: &PersistDelta,
) -> Result<()> {
    let sqlite_path = sqlite_persistence_path_for_json_path(path);
    let metadata_json = serde_json::to_string(&delta.metadata)
        .context("failed to serialize persisted state metadata")?;
    let connection = cache.connection_for(&sqlite_path)?;
    let tx = connection.transaction().with_context(|| {
        format!(
            "failed to start SQLite transaction for `{}`",
            sqlite_path.display()
        )
    })?;
    tx.execute(
        "INSERT INTO app_state(key, value_json) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        rusqlite::params![SQLITE_METADATA_KEY, metadata_json],
    )
    .with_context(|| {
        format!(
            "failed to write state metadata to `{}`",
            sqlite_path.display()
        )
    })?;
    for session_id in &delta.removed_session_ids {
        tx.execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
        )
        .with_context(|| {
            format!(
                "failed to remove session `{}` from `{}`",
                session_id,
                sqlite_path.display()
            )
        })?;
    }
    for session in &delta.changed_sessions {
        let session_json = serde_json::to_string(session)
            .context("failed to serialize persisted session")?;
        tx.execute(
            "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)
             ON CONFLICT(id) DO UPDATE SET value_json = excluded.value_json",
            rusqlite::params![&session.session.id, session_json],
        )
        .with_context(|| {
            format!(
                "failed to write persisted session `{}` to `{}`",
                session.session.id,
                sqlite_path.display()
            )
        })?;
    }
    tx.commit().with_context(|| {
        format!(
            "failed to commit persisted state to `{}`",
            sqlite_path.display()
        )
    })?;
    Ok(())
}

/// Persists state from a pre-built `PersistedState` snapshot.
#[cfg(not(test))]
fn persist_state_from_persisted(path: &FsPath, persisted: &PersistedState) -> Result<()> {
    persist_persisted_state_to_sqlite(&sqlite_persistence_path_for_json_path(path), persisted)
}

#[cfg(test)]
fn persist_state_from_persisted(path: &FsPath, persisted: &PersistedState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let encoded =
        serde_json::to_vec_pretty(persisted).context("failed to serialize persisted state")?;
    fs::write(path, encoded).with_context(|| format!("failed to write `{}`", path.display()))
}

/// Persists state directly from `StateInner` (used in tests for synchronous
/// setup of persisted state files).
#[cfg(test)]
fn persist_state(path: &FsPath, inner: &StateInner) -> Result<()> {
    let persisted = PersistedState::from_inner(inner);
    persist_state_from_persisted(path, &persisted)
}

