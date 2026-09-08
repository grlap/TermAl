// Physical transcript storage and differential writes. Large message bodies
// belong in a rowid table: WITHOUT ROWID stores them in the primary-key B-tree,
// where even a key comparison can allocate and read an entire overflow record.

fn ensure_sqlite_message_rowid_storage(connection: &rusqlite::Connection) -> Result<()> {
    let without_rowid: bool = connection
        .query_row(
            "SELECT wr FROM pragma_table_list('messages') WHERE schema = 'main'",
            [],
            |row| row.get(0),
        )
        .context("failed to inspect SQLite transcript storage layout")?;
    if !without_rowid {
        return Ok(());
    }
    if !connection.is_autocommit() {
        bail!("transcript storage conversion requires a connection outside a transaction");
    }

    // Follow SQLite's table-rebuild procedure. Disable foreign keys before
    // BEGIN so DROP cannot cascade into tables referencing messages; validate
    // the rebuilt relationships before COMMIT and restore enforcement on errors.
    let foreign_keys: bool = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    connection.pragma_update(None, "foreign_keys", false)?;
    let result = rebuild_sqlite_message_rowid_storage(connection);
    let restored = connection
        .pragma_update(None, "foreign_keys", foreign_keys)
        .context("failed to restore foreign key enforcement after transcript storage conversion");
    match (result, restored) {
        (Ok(()), restored) => restored,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(restore_error)) => Err(error.context(format!("{restore_error:#}"))),
    }
}

fn rebuild_sqlite_message_rowid_storage(connection: &rusqlite::Connection) -> Result<()> {
    let started = std::time::Instant::now();
    eprintln!("persist> transcript rowid conversion: waiting for writer lock");
    let tx =
        rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)
            .context("failed to begin transcript storage conversion")?;
    // Recheck after taking the writer lock: another connection may have already
    // completed the same maintenance while this connection waited for BEGIN.
    let without_rowid: bool = tx.query_row(
        "SELECT wr FROM pragma_table_list('messages') WHERE schema = 'main'",
        [],
        |row| row.get(0),
    )?;
    if !without_rowid {
        tx.commit()?;
        return Ok(());
    }
    let sql: String = tx.query_row(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'messages'",
        [],
        |row| row.get(0),
    )?;
    let open = sql
        .find('(')
        .context("missing transcript table column definition")?;
    let close = sql
        .rfind(')')
        .context("missing transcript table definition terminator")?;
    let suffix = sql[close + 1..]
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if close < open || !suffix.eq_ignore_ascii_case("WITHOUT ROWID") {
        bail!("unsupported transcript table options for rowid storage conversion");
    }
    // Retain the existing column definitions, constraints, and collations.
    // This is a physical layout change, not a reinterpretation of stored JSON.
    let create = format!("CREATE TABLE messages_rowid_rebuild {}", &sql[open..=close]);
    let objects = {
        let mut statement = tx.prepare(
            "SELECT type, name, sql FROM sqlite_schema
             WHERE sql IS NOT NULL AND
               ((type = 'index' AND tbl_name = 'messages') OR type IN ('trigger', 'view'))
             ORDER BY type, name",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    // Views and triggers on other tables may also reference messages. Remove
    // them transactionally during the rebuild so RENAME sees a valid schema,
    // and so copying data cannot fire application triggers.
    for (kind, name, _) in &objects {
        if kind == "trigger" || kind == "view" {
            tx.execute_batch(&format!("DROP {} \"{}\"", kind, name.replace('"', "\"\"")))?;
        }
    }
    eprintln!(
        "persist> converting transcript storage to rowid layout; preserving all message records"
    );
    tx.execute_batch(&create)
        .context("failed to create rowid transcript storage")?;
    let copied_rows = tx.execute(
        "INSERT INTO messages_rowid_rebuild(session_id, position, message_id, value_json, overview_kind, is_user)
         SELECT session_id, position, message_id, value_json, overview_kind, is_user FROM messages",
        [],
    ).context("failed to copy transcript records into rowid storage")?;
    eprintln!(
        "persist> transcript rowid conversion: copied {copied_rows} rows in {}ms; restoring schema and validating foreign keys",
        started.elapsed().as_millis()
    );
    tx.execute_batch(
        "DROP TABLE messages;
         ALTER TABLE messages_rowid_rebuild RENAME TO messages;",
    ).context("failed to replace transcript storage after copying records")?;
    // A view-owned INSTEAD OF trigger requires its target to exist. Restore
    // every view first, including views unrelated to the transcript table.
    for kind in ["index", "view", "trigger"] {
        for (_, _, sql) in objects.iter().filter(|(object_kind, _, _)| object_kind == kind) {
            tx.execute_batch(sql)
                .context("failed to restore transcript schema objects")?;
        }
    }
    let violation = tx.prepare("PRAGMA foreign_key_check")?.exists([])?;
    if violation {
        bail!(
            "foreign key validation failed during transcript storage conversion; original tables preserved"
        );
    }
    tx.commit()
        .context("failed to commit transcript storage conversion")?;
    connection.flush_prepared_statement_cache();
    eprintln!(
        "persist> transcript rowid storage conversion complete: {copied_rows} rows, elapsed={}ms",
        started.elapsed().as_millis()
    );
    Ok(())
}

fn write_serialized_persisted_messages(
    tx: &rusqlite::Transaction<'_>,
    session: &SerializedPersistedSession,
) -> Result<()> {
    let start = i64::try_from(session.message_start_index)
        .context("persisted transcript position exceeds SQLite integer range")?;
    let end = session
        .message_start_index
        .checked_add(session.messages.len())
        .context("persisted transcript position overflow")?;
    let end =
        i64::try_from(end).context("persisted transcript position exceeds SQLite integer range")?;

    // Remove only the truncated suffix. The cold prefix and retained messages
    // keep their rows, avoiding delete/insert amplification on every save.
    tx.execute(
        "DELETE FROM messages WHERE session_id = ?1 AND position >= ?2",
        rusqlite::params![session.session_id, end],
    )
    .context("failed to truncate persisted transcript")?;
    let identities = {
        let mut statement = tx.prepare_cached(
            "SELECT position, message_id FROM messages
             WHERE session_id = ?1 AND position >= ?2 AND position < ?3",
        )?;
        statement
            .query_map(rusqlite::params![session.session_id, start, end], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut remove =
        tx.prepare_cached("DELETE FROM messages WHERE session_id = ?1 AND position = ?2")?;
    // Release changed identities before any insert, including both sides of a
    // reorder, so UNIQUE(session_id, message_id) remains valid throughout.
    for (position, id) in identities {
        let offset = usize::try_from(position - start)?;
        if session.messages[offset].message_id != id {
            remove.execute(rusqlite::params![session.session_id, position])?;
        }
    }
    let mut upsert = tx.prepare_cached(
        "INSERT INTO messages(session_id, position, message_id, value_json, overview_kind, is_user)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(session_id, position) DO UPDATE SET
           message_id = excluded.message_id,
           value_json = excluded.value_json,
           overview_kind = excluded.overview_kind,
           is_user = excluded.is_user
         WHERE messages.message_id IS NOT excluded.message_id
            OR messages.value_json IS NOT excluded.value_json
            OR messages.overview_kind IS NOT excluded.overview_kind
            OR messages.is_user IS NOT excluded.is_user",
    ).context("failed to prepare differential transcript write")?;
    for message in &session.messages {
        upsert
            .execute(rusqlite::params![
                session.session_id,
                i64::try_from(message.position)
                    .context("persisted transcript position exceeds SQLite integer range")?,
                message.message_id,
                message.value_json,
                i64::try_from(conversation_overview_kind_index(message.overview_kind))?,
                i64::from(message.is_user),
            ])
            .with_context(|| {
                format!(
                    "failed to write transcript position {} for `{}`",
                    message.position, session.session_id,
                )
            })?;
    }
    Ok(())
}
