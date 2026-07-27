/*
Coordination SQLite schema and legacy migration.

Owns the independent `coordination.sqlite` path, schema initialization, and
one-time read-only migration from legacy coordination tables in
`termal.sqlite`. Runtime mailbox and board stores use these helpers but keep
separate long-lived connections and writer-admission domains.
*/

fn resolve_coordination_persistence_path(persistence_path: &FsPath) -> PathBuf {
    persistence_path.with_file_name("coordination.sqlite")
}

const COORDINATION_SQLITE_SCHEMA_VERSION: &str = "1";
const COORDINATION_LEGACY_IMPORT_KEY: &str = "legacy_coordination_import_v1";

fn ensure_sqlite_coordination_schema(connection: &rusqlite::Connection) -> Result<()> {
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS meta (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL
            );
            ",
        )
        .context("failed to initialize SQLite coordination meta schema")?;
    let stored_schema_version = match connection.query_row(
        "SELECT value FROM meta WHERE key = 'coordination_schema_version'",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(value) => Some(value),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(err) => return Err(err).context("failed to read SQLite coordination schema version"),
    };
    if let Some(stored_schema_version) = stored_schema_version.as_deref() {
        if stored_schema_version != COORDINATION_SQLITE_SCHEMA_VERSION {
            bail!(
                "unsupported SQLite coordination schema version `{}`; this binary supports `{}`",
                stored_schema_version,
                COORDINATION_SQLITE_SCHEMA_VERSION
            );
        }
    }
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS mailboxes (
              id TEXT PRIMARY KEY,
              participant_key TEXT NOT NULL UNIQUE,
              created_at TEXT NOT NULL,
              next_sequence INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS mailbox_participants (
              mailbox_id TEXT NOT NULL,
              session_id TEXT NOT NULL,
              display_name TEXT NOT NULL,
              processed_through INTEGER NOT NULL DEFAULT 0,
              joined_at TEXT NOT NULL,
              left_at TEXT,
              PRIMARY KEY (mailbox_id, session_id),
              FOREIGN KEY (mailbox_id) REFERENCES mailboxes(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS mailbox_messages (
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

            CREATE INDEX IF NOT EXISTS mailbox_participants_session
              ON mailbox_participants(session_id, left_at);

            CREATE TABLE IF NOT EXISTS coordination_board_scopes (
              scope_id TEXT PRIMARY KEY,
              generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0)
            );

            CREATE TABLE IF NOT EXISTS coordination_board_entries (
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

            CREATE TABLE IF NOT EXISTS coordination_board_history (
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

            CREATE TABLE IF NOT EXISTS coordination_board_idempotency (
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

            CREATE TABLE IF NOT EXISTS coordination_board_deleted_scopes (
              scope_id TEXT PRIMARY KEY,
              deleted_at TEXT NOT NULL
            );
            ",
        )
        .context("failed to initialize SQLite coordination schema")?;
    if !mailbox_messages_table_has_column(connection, "dispatch_outcome")? {
        connection
            .execute(
                "ALTER TABLE mailbox_messages ADD COLUMN dispatch_outcome TEXT",
                [],
            )
            .context("failed to add immutable mailbox dispatch outcome")?;
    }
    ensure_mailbox_dispatch_outcome_backfill(connection)?;
    connection
        .execute(
            "INSERT INTO meta(key, value) VALUES('coordination_schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![COORDINATION_SQLITE_SCHEMA_VERSION],
        )
        .context("failed to record SQLite coordination schema version")?;
    Ok(())
}

fn mailbox_dispatch_outcome_backfill_complete(
    connection: &rusqlite::Connection,
) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM meta
               WHERE key = 'mailbox_dispatch_outcome_backfill_v1'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .context("failed to inspect mailbox dispatch-outcome migration state")
}

fn ensure_mailbox_dispatch_outcome_backfill(
    connection: &rusqlite::Connection,
) -> Result<()> {
    // The completed path is read-only. Avoid taking SQLite's writer slot on
    // every connection schema check after this one-time migration has landed.
    if mailbox_dispatch_outcome_backfill_complete(connection)? {
        return Ok(());
    }

    // Re-check after acquiring IMMEDIATE: another connection may have completed
    // the migration between the read-only probe above and this transaction.
    // Check, backfill, and mark completion atomically. Production callers also
    // hold the path-scoped SQLite writer lock in
    // `ensure_sqlite_state_schema_for_path`, so separate connections cannot
    // race this one-time migration. The transaction prevents a crash between
    // the UPDATE and marker write from re-arming it on the next boot.
    let migration = rusqlite::Transaction::new_unchecked(
        connection,
        rusqlite::TransactionBehavior::Immediate,
    )
        .context("failed to begin mailbox dispatch-outcome migration")?;
    let dispatch_outcome_backfilled =
        mailbox_dispatch_outcome_backfill_complete(&migration)?;
    if !dispatch_outcome_backfilled {
        // The legacy lifecycle column cannot distinguish a direct delivery from a
        // delivery reached after recovery. Preserve deliveredToIdleSession as the
        // pragmatic immutable approximation, but normalize recovered/unknown
        // values to the accurate never-woken fallback. Pre-migration rows must not
        // be mined for recovery statistics.
        //
        // This is deliberately a one-time migration. Fresh appends use NULL as an
        // in-flight finalization marker, so repeating the backfill during ordinary
        // schema checks would fabricate a provisional immutable receipt.
        migration
            .execute(
                "UPDATE mailbox_messages
                 SET dispatch_outcome = CASE
                   WHEN notification_disposition = 'queuedBehindActiveTurn'
                     THEN 'queuedBehindActiveTurn'
                   WHEN notification_disposition = 'deliveredToIdleSession'
                     THEN 'deliveredToIdleSession'
                   ELSE 'durableButNotWoken'
                 END
                 WHERE dispatch_outcome IS NULL
                    OR dispatch_outcome NOT IN (
                      'durableButNotWoken',
                      'queuedBehindActiveTurn',
                      'deliveredToIdleSession'
                    )",
                [],
            )
            .context("failed to backfill immutable mailbox dispatch outcomes")?;
        migration
            .execute(
                "INSERT INTO meta(key, value)
                 VALUES('mailbox_dispatch_outcome_backfill_v1', 'complete')
                 ON CONFLICT(key) DO NOTHING",
                [],
            )
            .context("failed to record mailbox dispatch-outcome migration")?;
    }
    migration
        .commit()
        .context("failed to commit mailbox dispatch-outcome migration")?;
    Ok(())
}

fn mailbox_messages_table_has_column(
    connection: &rusqlite::Connection,
    column_name: &str,
) -> Result<bool> {
    let mut statement = connection.prepare("PRAGMA table_info(mailbox_messages)")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column_name {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_sqlite_coordination_schema_for_path(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<()> {
    let write_lock = sqlite_state_write_lock(path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    ensure_sqlite_coordination_schema(connection)
}

fn sqlite_read_only_uri(path: &FsPath) -> Result<String> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve legacy database `{}`", path.display()))?;
    let canonical = normalize_user_facing_path(&canonical);
    Ok(sqlite_read_only_uri_from_canonical_text(
        &canonical.to_string_lossy(),
    ))
}

fn sqlite_read_only_uri_from_canonical_text(canonical: &str) -> String {
    // Windows canonicalization commonly returns a verbatim drive path
    // (`\\?\C:\...`) or verbatim UNC path (`\\?\UNC\server\share\...`).
    // Feeding the slash-replaced form directly to SQLite would produce
    // `file://%3F/C:/...`, where `%3F` is parsed as an invalid URI authority.
    // Strip the verbatim marker before slash normalization. This runs on all
    // platforms so the URI contract remains directly testable without a
    // Windows-only CI host.
    let (normalized, is_unc) = if let Some(rest) = canonical.strip_prefix(r"\\?\UNC\") {
        (rest.replace('\\', "/"), true)
    } else if let Some(rest) = canonical.strip_prefix(r"\\?\") {
        (rest.replace('\\', "/"), false)
    } else if let Some(rest) = canonical.strip_prefix(r"\\") {
        (rest.replace('\\', "/"), true)
    } else {
        (canonical.replace('\\', "/"), false)
    };
    let mut uri = String::with_capacity(normalized.len() + 16);
    uri.push_str("file:");
    if is_unc {
        // A normal `file://server/share` URI gives `server` an authority role,
        // which bundled SQLite rejects unless compiled with the optional
        // SQLITE_ALLOW_URI_AUTHORITY flag. Keep the UNC's leading double slash
        // in the decoded path instead: the parser sees no authority, then
        // decodes this prefix to `//server/share` for the Windows VFS.
        uri.push_str("/%2F");
    } else if normalized.as_bytes().get(1) == Some(&b':')
        && normalized
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
    {
        // SQLite recognizes an absolute Windows drive only when the URI path
        // starts with `/X:/`.
        uri.push('/');
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in normalized.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~')
        {
            uri.push(char::from(byte));
        } else {
            uri.push('%');
            uri.push(char::from(HEX[usize::from(byte >> 4)]));
            uri.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    uri.push_str("?mode=ro");
    uri
}

fn legacy_coordination_table_exists(
    connection: &rusqlite::Connection,
    table_name: &str,
) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1
               FROM legacy_state.sqlite_master
               WHERE type = 'table' AND name = ?1
             )",
            rusqlite::params![table_name],
            |row| row.get::<_, bool>(0),
        )
        .with_context(|| format!("failed to inspect legacy coordination table `{table_name}`"))
}

fn legacy_mailbox_messages_has_dispatch_outcome(
    connection: &rusqlite::Connection,
) -> Result<bool> {
    let mut statement = connection
        .prepare("PRAGMA legacy_state.table_info(mailbox_messages)")
        .context("failed to inspect legacy mailbox message columns")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == "dispatch_outcome" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_coordination_migration_matches(
    transaction: &rusqlite::Transaction<'_>,
    label: &str,
    mismatch_query: &str,
) -> Result<()> {
    let mismatch_count = transaction
        .query_row(mismatch_query, [], |row| row.get::<_, u64>(0))
        .with_context(|| format!("failed to verify migrated {label}"))?;
    if mismatch_count != 0 {
        bail!(
            "legacy coordination migration verification failed for {label}: \
             {mismatch_count} mismatch row(s)"
        );
    }
    Ok(())
}

fn ensure_coordination_migration_destination_empty(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<()> {
    let row_count = transaction
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM main.mailboxes) +
               (SELECT COUNT(*) FROM main.mailbox_participants) +
               (SELECT COUNT(*) FROM main.mailbox_messages) +
               (SELECT COUNT(*) FROM main.coordination_board_scopes) +
               (SELECT COUNT(*) FROM main.coordination_board_entries) +
               (SELECT COUNT(*) FROM main.coordination_board_history) +
               (SELECT COUNT(*) FROM main.coordination_board_idempotency) +
               (SELECT COUNT(*) FROM main.coordination_board_deleted_scopes)",
            [],
            |row| row.get::<_, u64>(0),
        )
        .context("failed to inspect pre-marker coordination destination")?;
    if row_count != 0 {
        bail!(
            "legacy coordination migration requires an empty pre-marker destination, but found \
             {row_count} existing coordination row(s); refusing to merge independent histories"
        );
    }
    Ok(())
}

fn bootstrap_coordination_database(
    legacy_state_path: &FsPath,
    coordination_path: &FsPath,
) -> Result<()> {
    if let Some(parent) = coordination_path.parent() {
        create_local_state_directory(parent)?;
    }
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

    let write_lock = sqlite_state_write_lock(coordination_path);
    let _write_guard = lock_sqlite_state_writer(&write_lock);
    let migration_complete = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE key = ?1)",
            rusqlite::params![COORDINATION_LEGACY_IMPORT_KEY],
            |row| row.get::<_, bool>(0),
        )
        .context("failed to inspect legacy coordination migration marker")?;
    if migration_complete {
        return Ok(());
    }

    // A new installation has no legacy state file to import. Marking that fact
    // in the destination prevents every later boot from re-probing for legacy
    // tables while keeping marker ownership entirely in coordination.sqlite.
    if !legacy_state_path.exists() {
        connection
            .execute(
                "INSERT INTO meta(key, value) VALUES(?1, 'no-legacy-state')",
                rusqlite::params![COORDINATION_LEGACY_IMPORT_KEY],
            )
            .context("failed to record empty legacy coordination migration")?;
        verify_persist_commit_integrity(coordination_path)?;
        return Ok(());
    }

    reject_existing_sqlite_state_path_redirection(legacy_state_path)?;
    let legacy_uri = sqlite_read_only_uri(legacy_state_path)?;
    connection
        .execute(
            "ATTACH DATABASE ?1 AS legacy_state",
            rusqlite::params![legacy_uri],
        )
        .with_context(|| {
            format!(
                "failed to attach legacy state database `{}` read-only",
                legacy_state_path.display()
            )
        })?;

    let migration_result = (|| -> Result<()> {
        // The destination transaction is the only durable migration phase:
        // copy, verify, and marker commit together. The attached legacy file is
        // read-only and remains inert after cutover, so interruption leaves the
        // destination marker absent and the next boot can rerun safely.
        let transaction = rusqlite::Transaction::new_unchecked(
            &connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .context("failed to begin legacy coordination migration")?;
        let migration_complete = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM main.meta WHERE key = ?1)",
                rusqlite::params![COORDINATION_LEGACY_IMPORT_KEY],
                |row| row.get::<_, bool>(0),
            )
            .context("failed to recheck legacy coordination migration marker")?;
        if migration_complete {
            transaction
                .commit()
                .context("failed to finish concurrent coordination migration check")?;
            return Ok(());
        }
        // The marker is the cutover boundary. Before it exists, the
        // destination must be pristine: merging independently-created rows
        // could silently keep a conflicting payload under the same primary
        // key. Ordinary INSERTs below then make every collision fail the whole
        // copy transaction instead of hiding it behind OR IGNORE.
        ensure_coordination_migration_destination_empty(&transaction)?;

        let has_mailboxes = legacy_coordination_table_exists(&transaction, "mailboxes")?;
        let has_mailbox_participants =
            legacy_coordination_table_exists(&transaction, "mailbox_participants")?;
        let has_mailbox_messages =
            legacy_coordination_table_exists(&transaction, "mailbox_messages")?;
        let has_board_scopes =
            legacy_coordination_table_exists(&transaction, "coordination_board_scopes")?;
        let has_board_entries =
            legacy_coordination_table_exists(&transaction, "coordination_board_entries")?;
        let has_board_history =
            legacy_coordination_table_exists(&transaction, "coordination_board_history")?;
        let has_board_idempotency =
            legacy_coordination_table_exists(&transaction, "coordination_board_idempotency")?;
        let has_board_deleted_scopes = legacy_coordination_table_exists(
            &transaction,
            "coordination_board_deleted_scopes",
        )?;

        if has_mailboxes || has_mailbox_participants || has_mailbox_messages {
            if !(has_mailboxes && has_mailbox_participants && has_mailbox_messages) {
                bail!("legacy mailbox schema is incomplete; refusing a partial migration");
            }
            transaction
                .execute_batch(
                    "
                    INSERT INTO main.mailboxes(
                      id, participant_key, created_at, next_sequence
                    )
                    SELECT id, participant_key, created_at, next_sequence
                    FROM legacy_state.mailboxes;

                    INSERT INTO main.mailbox_participants(
                      mailbox_id, session_id, display_name, processed_through,
                      joined_at, left_at
                    )
                    SELECT mailbox_id, session_id, display_name, processed_through,
                           joined_at, left_at
                    FROM legacy_state.mailbox_participants;
                    ",
                )
                .context("failed to copy legacy mailbox metadata")?;
            let source_dispatch_outcome = if legacy_mailbox_messages_has_dispatch_outcome(
                &transaction,
            )? {
                "CASE
                   WHEN dispatch_outcome IN (
                     'durableButNotWoken',
                     'queuedBehindActiveTurn',
                     'deliveredToIdleSession'
                   ) THEN dispatch_outcome
                   WHEN notification_disposition = 'queuedBehindActiveTurn'
                     THEN 'queuedBehindActiveTurn'
                   WHEN notification_disposition = 'deliveredToIdleSession'
                     THEN 'deliveredToIdleSession'
                   ELSE 'durableButNotWoken'
                 END"
            } else {
                "CASE
                   WHEN notification_disposition = 'queuedBehindActiveTurn'
                     THEN 'queuedBehindActiveTurn'
                   WHEN notification_disposition = 'deliveredToIdleSession'
                     THEN 'deliveredToIdleSession'
                   ELSE 'durableButNotWoken'
                 END"
            };
            transaction
                .execute_batch(&format!(
                    "
                    INSERT INTO main.mailbox_messages(
                      id, mailbox_id, sequence, sender_session_id, sender_name,
                      target_session_id, target_name, created_at, class, topic,
                      state_stamp, body, idempotency_key, unread_depth_at_append,
                      notification_disposition, dispatch_outcome
                    )
                    SELECT id, mailbox_id, sequence, sender_session_id, sender_name,
                           target_session_id, target_name, created_at, class, topic,
                           state_stamp, body, idempotency_key, unread_depth_at_append,
                           notification_disposition, {source_dispatch_outcome}
                    FROM legacy_state.mailbox_messages;
                    "
                ))
                .context("failed to copy legacy mailbox messages")?;

            ensure_coordination_migration_matches(
                &transaction,
                "mailbox heads",
                "
                SELECT
                  (SELECT COUNT(*) FROM (
                    SELECT id, next_sequence
                    FROM legacy_state.mailboxes
                    EXCEPT
                    SELECT id, next_sequence
                    FROM main.mailboxes
                  )) +
                  (SELECT COUNT(*) FROM (
                    SELECT id, next_sequence
                    FROM main.mailboxes
                    EXCEPT
                    SELECT id, next_sequence
                    FROM legacy_state.mailboxes
                  ))
                ",
            )?;
            ensure_coordination_migration_matches(
                &transaction,
                "mailbox participant cursors",
                "
                SELECT
                  (SELECT COUNT(*) FROM (
                    SELECT mailbox_id, session_id, processed_through
                    FROM legacy_state.mailbox_participants
                    EXCEPT
                    SELECT mailbox_id, session_id, processed_through
                    FROM main.mailbox_participants
                  )) +
                  (SELECT COUNT(*) FROM (
                    SELECT mailbox_id, session_id, processed_through
                    FROM main.mailbox_participants
                    EXCEPT
                    SELECT mailbox_id, session_id, processed_through
                    FROM legacy_state.mailbox_participants
                  ))
                ",
            )?;
            ensure_coordination_migration_matches(
                &transaction,
                "per-mailbox message counts and maximum sequences",
                "
                SELECT
                  (SELECT COUNT(*) FROM (
                    SELECT mailbox_id, COUNT(*) AS row_count,
                           COALESCE(MAX(sequence), 0) AS max_sequence
                    FROM legacy_state.mailbox_messages
                    GROUP BY mailbox_id
                    EXCEPT
                    SELECT mailbox_id, COUNT(*) AS row_count,
                           COALESCE(MAX(sequence), 0) AS max_sequence
                    FROM main.mailbox_messages
                    GROUP BY mailbox_id
                  )) +
                  (SELECT COUNT(*) FROM (
                    SELECT mailbox_id, COUNT(*) AS row_count,
                           COALESCE(MAX(sequence), 0) AS max_sequence
                    FROM main.mailbox_messages
                    GROUP BY mailbox_id
                    EXCEPT
                    SELECT mailbox_id, COUNT(*) AS row_count,
                           COALESCE(MAX(sequence), 0) AS max_sequence
                    FROM legacy_state.mailbox_messages
                    GROUP BY mailbox_id
                  ))
                ",
            )?;
            ensure_coordination_migration_matches(
                &transaction,
                "mailbox idempotency counts",
                "
                SELECT
                  (SELECT COUNT(*) FROM (
                    SELECT sender_session_id, COUNT(*)
                    FROM legacy_state.mailbox_messages
                    GROUP BY sender_session_id
                    EXCEPT
                    SELECT sender_session_id, COUNT(*)
                    FROM main.mailbox_messages
                    GROUP BY sender_session_id
                  )) +
                  (SELECT COUNT(*) FROM (
                    SELECT sender_session_id, COUNT(*)
                    FROM main.mailbox_messages
                    GROUP BY sender_session_id
                    EXCEPT
                    SELECT sender_session_id, COUNT(*)
                    FROM legacy_state.mailbox_messages
                    GROUP BY sender_session_id
                  ))
                ",
            )?;
        } else {
            ensure_coordination_migration_matches(
                &transaction,
                "empty legacy mailbox set",
                "SELECT
                   (SELECT COUNT(*) FROM main.mailboxes) +
                   (SELECT COUNT(*) FROM main.mailbox_participants) +
                   (SELECT COUNT(*) FROM main.mailbox_messages)",
            )?;
        }

        if has_board_scopes
            || has_board_entries
            || has_board_history
            || has_board_idempotency
            || has_board_deleted_scopes
        {
            if !(has_board_scopes
                && has_board_entries
                && has_board_history
                && has_board_idempotency)
            {
                bail!("legacy coordination-board schema is incomplete; refusing a partial migration");
            }
            transaction
                .execute_batch(
                    "
                    INSERT INTO main.coordination_board_scopes(scope_id, generation)
                    SELECT scope_id, generation
                    FROM legacy_state.coordination_board_scopes;

                    INSERT INTO main.coordination_board_entries(
                      scope_id, key, revision, generation, value_json,
                      author_session_id, author_name, updated_at, state_stamp
                    )
                    SELECT scope_id, key, revision, generation, value_json,
                           author_session_id, author_name, updated_at, state_stamp
                    FROM legacy_state.coordination_board_entries;

                    INSERT INTO main.coordination_board_history(
                      scope_id, key, revision, generation, value_json,
                      author_session_id, author_name, updated_at, state_stamp
                    )
                    SELECT scope_id, key, revision, generation, value_json,
                           author_session_id, author_name, updated_at, state_stamp
                    FROM legacy_state.coordination_board_history;

                    INSERT INTO main.coordination_board_idempotency(
                      scope_id, author_session_id, idempotency_key,
                      request_hash, receipt_json, created_at
                    )
                    SELECT scope_id, author_session_id, idempotency_key,
                           request_hash, receipt_json, created_at
                    FROM legacy_state.coordination_board_idempotency;
                    ",
                )
                .context("failed to copy legacy coordination-board rows")?;
            if has_board_deleted_scopes {
                transaction
                    .execute_batch(
                        "
                        INSERT INTO main.coordination_board_deleted_scopes(
                          scope_id, deleted_at
                        )
                        SELECT scope_id, deleted_at
                        FROM legacy_state.coordination_board_deleted_scopes;
                        ",
                    )
                    .context("failed to copy legacy coordination-board deletion fences")?;
            }

            ensure_coordination_migration_matches(
                &transaction,
                "coordination-board scope generations",
                "
                SELECT
                  (SELECT COUNT(*) FROM (
                    SELECT scope_id, generation
                    FROM legacy_state.coordination_board_scopes
                    EXCEPT
                    SELECT scope_id, generation
                    FROM main.coordination_board_scopes
                  )) +
                  (SELECT COUNT(*) FROM (
                    SELECT scope_id, generation
                    FROM main.coordination_board_scopes
                    EXCEPT
                    SELECT scope_id, generation
                    FROM legacy_state.coordination_board_scopes
                  ))
                ",
            )?;
            ensure_coordination_migration_matches(
                &transaction,
                "coordination-board heads including tombstones",
                "
                SELECT
                  (SELECT COUNT(*) FROM (
                    SELECT scope_id, key, revision, generation,
                           value_json IS NULL AS deleted
                    FROM legacy_state.coordination_board_entries
                    EXCEPT
                    SELECT scope_id, key, revision, generation,
                           value_json IS NULL AS deleted
                    FROM main.coordination_board_entries
                  )) +
                  (SELECT COUNT(*) FROM (
                    SELECT scope_id, key, revision, generation,
                           value_json IS NULL AS deleted
                    FROM main.coordination_board_entries
                    EXCEPT
                    SELECT scope_id, key, revision, generation,
                           value_json IS NULL AS deleted
                    FROM legacy_state.coordination_board_entries
                  ))
                ",
            )?;
            ensure_coordination_migration_matches(
                &transaction,
                "coordination-board history",
                "
                SELECT
                  (SELECT COUNT(*) FROM (
                    SELECT scope_id, key, COUNT(*), MAX(revision), MAX(generation)
                    FROM legacy_state.coordination_board_history
                    GROUP BY scope_id, key
                    EXCEPT
                    SELECT scope_id, key, COUNT(*), MAX(revision), MAX(generation)
                    FROM main.coordination_board_history
                    GROUP BY scope_id, key
                  )) +
                  (SELECT COUNT(*) FROM (
                    SELECT scope_id, key, COUNT(*), MAX(revision), MAX(generation)
                    FROM main.coordination_board_history
                    GROUP BY scope_id, key
                    EXCEPT
                    SELECT scope_id, key, COUNT(*), MAX(revision), MAX(generation)
                    FROM legacy_state.coordination_board_history
                    GROUP BY scope_id, key
                  ))
                ",
            )?;
            ensure_coordination_migration_matches(
                &transaction,
                "coordination-board idempotency rows",
                "
                SELECT
                  (SELECT COUNT(*) FROM (
                    SELECT scope_id, author_session_id, COUNT(*)
                    FROM legacy_state.coordination_board_idempotency
                    GROUP BY scope_id, author_session_id
                    EXCEPT
                    SELECT scope_id, author_session_id, COUNT(*)
                    FROM main.coordination_board_idempotency
                    GROUP BY scope_id, author_session_id
                  )) +
                  (SELECT COUNT(*) FROM (
                    SELECT scope_id, author_session_id, COUNT(*)
                    FROM main.coordination_board_idempotency
                    GROUP BY scope_id, author_session_id
                    EXCEPT
                    SELECT scope_id, author_session_id, COUNT(*)
                    FROM legacy_state.coordination_board_idempotency
                    GROUP BY scope_id, author_session_id
                  ))
                ",
            )?;
            if has_board_deleted_scopes {
                ensure_coordination_migration_matches(
                    &transaction,
                    "coordination-board deletion fences",
                    "
                    SELECT
                      (SELECT COUNT(*) FROM (
                        SELECT scope_id, deleted_at
                        FROM legacy_state.coordination_board_deleted_scopes
                        EXCEPT
                        SELECT scope_id, deleted_at
                        FROM main.coordination_board_deleted_scopes
                      )) +
                      (SELECT COUNT(*) FROM (
                        SELECT scope_id, deleted_at
                        FROM main.coordination_board_deleted_scopes
                        EXCEPT
                        SELECT scope_id, deleted_at
                        FROM legacy_state.coordination_board_deleted_scopes
                      ))
                    ",
                )?;
            }
        } else {
            ensure_coordination_migration_matches(
                &transaction,
                "empty legacy coordination-board set",
                "SELECT
                   (SELECT COUNT(*) FROM main.coordination_board_scopes) +
                   (SELECT COUNT(*) FROM main.coordination_board_entries) +
                   (SELECT COUNT(*) FROM main.coordination_board_history) +
                   (SELECT COUNT(*) FROM main.coordination_board_idempotency) +
                   (SELECT COUNT(*) FROM main.coordination_board_deleted_scopes)",
            )?;
        }

        transaction
            .execute(
                "INSERT INTO main.meta(key, value) VALUES(?1, ?2)",
                rusqlite::params![
                    COORDINATION_LEGACY_IMPORT_KEY,
                    legacy_state_path.display().to_string()
                ],
            )
            .context("failed to record completed legacy coordination migration")?;
        transaction
            .commit()
            .context("failed to commit legacy coordination migration")?;
        Ok(())
    })();

    let detach_result = connection
        .execute_batch("DETACH DATABASE legacy_state")
        .context("failed to detach legacy state database");
    migration_result?;
    detach_result?;
    verify_persist_commit_integrity(coordination_path)?;
    Ok(())
}

#[cfg(test)]
mod sqlite_coordination_tests {
    use super::*;

    #[test]
    fn coordination_schema_adds_and_backfills_immutable_mailbox_dispatch_outcome() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        connection
            .execute_batch(
                "
                CREATE TABLE meta (
                  key TEXT PRIMARY KEY,
                  value TEXT NOT NULL
                );
                CREATE TABLE mailbox_messages (
                  id TEXT PRIMARY KEY,
                  notification_disposition TEXT NOT NULL
                );
                INSERT INTO mailbox_messages(id, notification_disposition)
                VALUES
                  ('mailbox-message-durable', 'durableButNotWoken'),
                  ('mailbox-message-queued', 'queuedBehindActiveTurn'),
                  ('mailbox-message-recovered', 'recoveredWake'),
                  ('mailbox-message-delivered', 'deliveredToIdleSession'),
                  ('mailbox-message-unknown', 'legacyUnknownState');
                ",
            )
            .expect("seed pre-dispatch-outcome mailbox schema");
        connection
            .execute(
                "INSERT INTO meta(key, value) VALUES('coordination_schema_version', ?1)",
                rusqlite::params![COORDINATION_SQLITE_SCHEMA_VERSION],
            )
            .expect("seed supported schema version");

        ensure_sqlite_coordination_schema(&connection).expect("schema migration should succeed");

        assert!(
            mailbox_messages_table_has_column(&connection, "dispatch_outcome")
                .expect("mailbox column should be inspectable")
        );
        let mut statement = connection
            .prepare(
                "SELECT id, dispatch_outcome
                 FROM mailbox_messages
                 ORDER BY id",
            )
            .expect("backfilled outcomes should prepare");
        let outcomes = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("backfilled outcomes should query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("backfilled outcomes should decode");
        assert_eq!(
            outcomes,
            vec![
                (
                    "mailbox-message-delivered".to_owned(),
                    "deliveredToIdleSession".to_owned(),
                ),
                (
                    "mailbox-message-durable".to_owned(),
                    "durableButNotWoken".to_owned(),
                ),
                (
                    "mailbox-message-queued".to_owned(),
                    "queuedBehindActiveTurn".to_owned(),
                ),
                (
                    "mailbox-message-recovered".to_owned(),
                    "durableButNotWoken".to_owned(),
                ),
                (
                    "mailbox-message-unknown".to_owned(),
                    "durableButNotWoken".to_owned(),
                ),
            ]
        );
        drop(statement);

        connection
            .execute_batch(
                "
                UPDATE mailbox_messages
                SET dispatch_outcome = NULL,
                    notification_disposition = 'queuedBehindActiveTurn'
                WHERE id = 'mailbox-message-durable';
                ",
            )
            .expect("seed a fresh provisional outcome after migration");
        ensure_sqlite_coordination_schema(&connection)
            .expect("ordinary schema check should repeat");
        let provisional_outcome: Option<String> = connection
            .query_row(
                "SELECT dispatch_outcome
                 FROM mailbox_messages
                 WHERE id = 'mailbox-message-durable'",
                [],
                |row| row.get(0),
            )
            .expect("provisional outcome should read");
        assert_eq!(
            provisional_outcome, None,
            "one-time migration must not rewrite a fresh in-flight receipt marker"
        );
    }

    #[test]
    fn completed_dispatch_outcome_backfill_fast_path_is_read_only() {
        let connection =
            rusqlite::Connection::open_in_memory().expect("in-memory sqlite should open");
        ensure_sqlite_coordination_schema(&connection)
            .expect("initial schema migration should succeed");
        connection
            .execute_batch("PRAGMA query_only = ON;")
            .expect("test connection should enter query-only mode");

        ensure_mailbox_dispatch_outcome_backfill(&connection)
            .expect("completed migration should use the read-only fast path");
    }

    #[test]
    fn coordination_schema_dispatch_outcome_migration_is_safe_across_connections() {
        let root = std::env::temp_dir().join(format!(
            "termal-schema-concurrency-{}",
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
                        .expect("concurrent schema ensure should succeed");
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().expect("schema ensure thread should join");
        }

        let connection =
            rusqlite::Connection::open(&path).expect("schema database should reopen");
        let marker_count: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM meta
                 WHERE key = 'mailbox_dispatch_outcome_backfill_v1'",
                [],
                |row| row.get(0),
            )
            .expect("migration marker should read");
        assert_eq!(marker_count, 1);
        drop(connection);
        fs::remove_dir_all(root).expect("test directory should clean up");
    }

    #[test]
    fn sqlite_read_only_uri_normalizes_windows_verbatim_drive_and_unc_paths() {
        assert_eq!(
            sqlite_read_only_uri_from_canonical_text(
                r"\\?\C:\Users\Greg Lapinski\TermAl#state.sqlite"
            ),
            "file:/C:/Users/Greg%20Lapinski/TermAl%23state.sqlite?mode=ro"
        );
        assert_eq!(
            sqlite_read_only_uri_from_canonical_text(
                r"\\?\UNC\server\shared folder\TermAl?state.sqlite"
            ),
            "file:/%2Fserver/shared%20folder/TermAl%3Fstate.sqlite?mode=ro"
        );
        assert_eq!(
            sqlite_read_only_uri_from_canonical_text(
                r"\\server\shared folder\TermAl?state.sqlite"
            ),
            "file:/%2Fserver/shared%20folder/TermAl%3Fstate.sqlite?mode=ro"
        );
    }

    #[test]
    fn legacy_coordination_migration_preserves_mailbox_board_and_marker_fast_path() {
        let root = std::env::temp_dir().join(format!(
            "termal-coordination-migration-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("migration test root should exist");
        let legacy_state_path = root.join("termal.sqlite");
        let coordination_path = resolve_coordination_persistence_path(&legacy_state_path);
        {
            let connection = open_sqlite_state_connection(&legacy_state_path)
                .expect("legacy state database should open");
            ensure_sqlite_state_schema_for_path(&connection, &legacy_state_path)
                .expect("legacy state schema should initialize");
        }
        let legacy_mailboxes =
            MailboxStore::open(&legacy_state_path).expect("legacy mailbox store should open");
        let appended = legacy_mailboxes
            .append(&MailboxAppendInput {
                sender_session_id: "session-migrate-sender".to_owned(),
                sender_name: "Sender".to_owned(),
                target_session_id: "session-migrate-target".to_owned(),
                target_name: "Target".to_owned(),
                body: "Preserve this durable body byte-for-byte.".to_owned(),
                idempotency_key: "legacy-mailbox-send-1".to_owned(),
                topic: Some("migration".to_owned()),
                state_stamp: Some("legacy-state".to_owned()),
            })
            .expect("legacy mailbox append should succeed");
        legacy_mailboxes
            .record_initial_dispatch_outcome(
                &appended.message_id,
                "queuedBehindActiveTurn",
            )
            .expect("legacy dispatch outcome should finalize");
        let acknowledged = legacy_mailboxes
            .acknowledge(
                "session-migrate-target",
                &appended.mailbox_id,
                0,
                appended.sequence,
            )
            .expect("legacy cursor should advance");
        assert_eq!(
            acknowledged
                .participants
                .iter()
                .find(|participant| participant.session_id == "session-migrate-target")
                .expect("acknowledged target participant should exist")
                .processed_through,
            1
        );

        let legacy_board = CoordinationBoardStore::open(&legacy_state_path)
            .expect("legacy board store should open");
        let board_created = legacy_board
            .set(&CoordinationBoardSetInput {
                scope_project_id: "project-migrate".to_owned(),
                key: "freeze.current".to_owned(),
                value: Some(json!({"digest": "abc"})),
                expected_revision: 0,
                author_session_id: "session-migrate-sender".to_owned(),
                author_name: "Sender".to_owned(),
                idempotency_key: "legacy-board-set-1".to_owned(),
                state_stamp: Some("legacy-state".to_owned()),
            })
            .expect("legacy board create should succeed");
        let board_deleted = legacy_board
            .set(&CoordinationBoardSetInput {
                scope_project_id: "project-migrate".to_owned(),
                key: "freeze.current".to_owned(),
                value: None,
                expected_revision: board_created.revision,
                author_session_id: "session-migrate-sender".to_owned(),
                author_name: "Sender".to_owned(),
                idempotency_key: "legacy-board-delete-1".to_owned(),
                state_stamp: Some("legacy-state".to_owned()),
            })
            .expect("legacy board delete should create a tombstone");
        assert_eq!(board_deleted.revision, 2);
        legacy_board
            .set(&CoordinationBoardSetInput {
                scope_project_id: "project-migrate-deleted".to_owned(),
                key: "status.before-delete".to_owned(),
                value: Some(json!("present")),
                expected_revision: 0,
                author_session_id: "session-migrate-sender".to_owned(),
                author_name: "Sender".to_owned(),
                idempotency_key: "legacy-board-fence-seed".to_owned(),
                state_stamp: Some("legacy-state".to_owned()),
            })
            .expect("legacy deleted-scope fixture should persist");
        assert!(
            legacy_board
                .delete_scope_for_project_lifecycle("project-migrate-deleted")
                .expect("legacy deleted scope should install its fence"),
            "the first lifecycle delete should create a fence"
        );
        drop(legacy_board);

        bootstrap_coordination_database(&legacy_state_path, &coordination_path)
            .expect("legacy coordination data should migrate");
        let migrated_mailboxes =
            MailboxStore::open(&coordination_path).expect("migrated mailbox store should open");
        let summaries = migrated_mailboxes
            .list_for_session("session-migrate-target")
            .expect("migrated mailbox should list");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].id, appended.mailbox_id);
        assert_eq!(summaries[0].latest_sequence, 1);
        assert_eq!(
            summaries[0]
                .participants
                .iter()
                .find(|participant| participant.session_id == "session-migrate-target")
                .expect("migrated target participant should exist")
                .processed_through,
            1
        );
        let migrated_message = migrated_mailboxes
            .read_message("session-migrate-target", &appended.message_id)
            .expect("migrated message should read");
        assert_eq!(
            migrated_message.body,
            "Preserve this durable body byte-for-byte."
        );
        assert_eq!(
            migrated_message.notification_state,
            "queuedBehindActiveTurn"
        );

        let migrated_board = CoordinationBoardStore::open(&coordination_path)
            .expect("migrated board store should open");
        let board_page = migrated_board
            .list(&CoordinationBoardListRequest {
                scope_project_id: "project-migrate".to_owned(),
                ..CoordinationBoardListRequest::default()
            })
            .expect("migrated board should list");
        assert_eq!(board_page.generation, 2);
        assert!(board_page.entries.is_empty(), "tombstones stay hidden");
        let board_head = migrated_board
            .connection()
            .expect("migrated board connection should open")
            .query_row(
                "SELECT revision, generation, value_json
                 FROM coordination_board_entries
                 WHERE scope_id = 'project-migrate' AND key = 'freeze.current'",
                [],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .expect("migrated tombstone should remain durable");
        assert_eq!(board_head, (2, 2, None));
        let deleted_scope_error = migrated_board
            .get("project-migrate-deleted", "status.before-delete")
            .expect_err("migration must preserve permanent project-deletion fences");
        assert!(
            deleted_scope_error
                .downcast_ref::<CoordinationBoardStoreError>()
                .is_some_and(|error| {
                    error.kind == CoordinationBoardStoreErrorKind::NotFound
                        && error.message.contains("was deleted with its project")
                }),
            "migrated deletion fence should remain authoritative: {deleted_scope_error:#}"
        );

        let destination = rusqlite::Connection::open(&coordination_path)
            .expect("destination should reopen");
        let marker_count: u32 = destination
            .query_row(
                "SELECT COUNT(*) FROM meta WHERE key = ?1",
                rusqlite::params![COORDINATION_LEGACY_IMPORT_KEY],
                |row| row.get(0),
            )
            .expect("migration marker should read");
        assert_eq!(marker_count, 1);
        drop(destination);

        let later_legacy = legacy_mailboxes
            .append(&MailboxAppendInput {
                sender_session_id: "session-migrate-sender".to_owned(),
                sender_name: "Sender".to_owned(),
                target_session_id: "session-migrate-target".to_owned(),
                target_name: "Target".to_owned(),
                body: "Marker fast path must not import this later row.".to_owned(),
                idempotency_key: "legacy-mailbox-send-after-marker".to_owned(),
                topic: Some("migration".to_owned()),
                state_stamp: None,
            })
            .expect("legacy source should remain inert but readable/writable in the test");
        assert_eq!(later_legacy.sequence, 2);
        drop(later_legacy);
        drop(legacy_mailboxes);
        drop(migrated_board);
        drop(migrated_mailboxes);

        bootstrap_coordination_database(&legacy_state_path, &coordination_path)
            .expect("marker fast path should be a no-op");
        let reopened =
            MailboxStore::open(&coordination_path).expect("destination should reopen");
        let summaries = reopened
            .list_for_session("session-migrate-target")
            .expect("destination mailbox should list");
        assert_eq!(
            summaries[0].latest_sequence, 1,
            "marker-present boot must never re-import inert legacy rows"
        );
        drop(reopened);
        fs::remove_dir_all(root).expect("migration test root should clean up");
    }

    #[test]
    fn coordination_migration_rejects_same_key_conflicting_destination_payload() {
        let root = std::env::temp_dir().join(format!(
            "termal-coordination-migration-rollback-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&root).expect("migration rollback root should exist");
        let legacy_state_path = root.join("termal.sqlite");
        let coordination_path = resolve_coordination_persistence_path(&legacy_state_path);
        let legacy_store =
            MailboxStore::open(&legacy_state_path).expect("legacy mailbox store should open");
        let legacy_message = legacy_store
            .append(&MailboxAppendInput {
                sender_session_id: "session-rollback-sender".to_owned(),
                sender_name: "Sender".to_owned(),
                target_session_id: "session-rollback-target".to_owned(),
                target_name: "Target".to_owned(),
                body: "Rollback source".to_owned(),
                idempotency_key: "rollback-source".to_owned(),
                topic: None,
                state_stamp: None,
            })
            .expect("legacy message should append");
        let legacy_mailbox_id = legacy_message.mailbox_id.clone();
        let legacy_message_id = legacy_message.message_id.clone();
        drop(legacy_message);
        drop(legacy_store);

        let destination =
            MailboxStore::open(&coordination_path).expect("destination schema should initialize");
        let conflicting = destination
            .append(&MailboxAppendInput {
                sender_session_id: "session-rollback-sender".to_owned(),
                sender_name: "Sender".to_owned(),
                target_session_id: "session-rollback-target".to_owned(),
                target_name: "Target".to_owned(),
                body: "Conflicting destination payload".to_owned(),
                idempotency_key: "destination-conflict".to_owned(),
                topic: None,
                state_stamp: None,
            })
            .expect("destination conflict seed should append");
        let conflicting_mailbox_id = conflicting.mailbox_id.clone();
        let conflicting_message_id = conflicting.message_id.clone();
        drop(conflicting);
        drop(destination);
        let destination_connection = rusqlite::Connection::open(&coordination_path)
            .expect("destination conflict should reopen");
        destination_connection
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .expect("fixture should allow atomic identifier rewrites");
        destination_connection
            .execute(
                "UPDATE mailbox_participants SET mailbox_id = ?1 WHERE mailbox_id = ?2",
                rusqlite::params![&legacy_mailbox_id, &conflicting_mailbox_id],
            )
            .expect("destination participant keys should match the source mailbox");
        destination_connection
            .execute(
                "UPDATE mailbox_messages
                 SET id = ?1, mailbox_id = ?2, idempotency_key = 'rollback-source'
                 WHERE id = ?3",
                rusqlite::params![
                    &legacy_message_id,
                    &legacy_mailbox_id,
                    &conflicting_message_id
                ],
            )
            .expect("destination row should share the source message primary key");
        destination_connection
            .execute(
                "UPDATE mailboxes SET id = ?1 WHERE id = ?2",
                rusqlite::params![&legacy_mailbox_id, &conflicting_mailbox_id],
            )
            .expect("destination mailbox should share the source primary key");
        let foreign_key_violations: u32 = destination_connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .expect("rewritten destination fixture should pass foreign-key validation");
        assert_eq!(foreign_key_violations, 0);
        drop(destination_connection);

        let error = bootstrap_coordination_database(&legacy_state_path, &coordination_path)
            .expect_err("migration must reject a conflicting pre-marker destination");
        assert!(
            format!("{error:#}").contains("requires an empty pre-marker destination"),
            "{error:#}"
        );
        let connection = rusqlite::Connection::open(&coordination_path)
            .expect("rejected destination should reopen");
        let marker_count: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM meta WHERE key = ?1",
                rusqlite::params![COORDINATION_LEGACY_IMPORT_KEY],
                |row| row.get(0),
            )
            .expect("rolled-back marker count should read");
        let retained_body: String = connection
            .query_row(
                "SELECT body FROM mailbox_messages WHERE id = ?1",
                rusqlite::params![&legacy_message_id],
                |row| row.get(0),
            )
            .expect("pre-marker conflicting payload should remain untouched");
        assert_eq!(marker_count, 0);
        assert_eq!(retained_body, "Conflicting destination payload");
        drop(connection);
        fs::remove_dir_all(root).expect("migration rollback root should clean up");
    }
}
