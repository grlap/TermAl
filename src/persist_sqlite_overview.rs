// Compatibility repair for the retained, unused transcript-overview columns.
// Schema inventory and defaults stay unchanged; existing values are never backfilled.

fn ensure_sqlite_message_overview_columns(connection: &rusqlite::Connection) -> Result<()> {
    let existing_columns = {
        let mut statement = connection
            .prepare("PRAGMA table_info(messages)")
            .context("failed to inspect SQLite transcript columns")?;
        statement
            .query_map([], |row| row.get::<_, String>(1))
            .context("failed to query SQLite transcript columns")?
            .collect::<rusqlite::Result<HashSet<_>>>()
            .context("failed to read SQLite transcript columns")?
    };
    let missing_kind = !existing_columns.contains("overview_kind");
    let missing_user = !existing_columns.contains("is_user");
    if missing_kind {
        connection
            .execute(
                "ALTER TABLE messages
                 ADD COLUMN overview_kind INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .context("failed to add SQLite transcript overview kind")?;
    }
    if missing_user {
        connection
            .execute(
                "ALTER TABLE messages
                 ADD COLUMN is_user INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .context("failed to add SQLite transcript overview author flag")?;
    }
    Ok(())
}
