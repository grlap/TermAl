// SQLite transcript-overview encoding, schema upgrades, and backfill.
//
// Extracted from `persist.rs`; this include fragment keeps overview-specific
// schema lifecycle work out of the core persistence connection/load/write
// module.

fn encode_conversation_overview_message(
    kind: ConversationOverviewKind,
    is_user: bool,
) -> u8 {
    u8::try_from(conversation_overview_kind_index(kind)).unwrap_or_default()
        | (u8::from(is_user) << 2)
}

fn decode_conversation_overview_message(
    encoded: u8,
) -> Result<(ConversationOverviewKind, bool)> {
    if encoded & !0b111 != 0 {
        bail!("conversation overview message byte {encoded} has unsupported flags");
    }
    let kind = match encoded & 0b11 {
        0 => ConversationOverviewKind::Text,
        1 => ConversationOverviewKind::Command,
        2 => ConversationOverviewKind::Diff,
        3 => ConversationOverviewKind::Error,
        _ => unreachable!("two-bit overview kind is exhaustive"),
    };
    Ok((kind, encoded & 0b100 != 0))
}

fn ensure_sqlite_message_overview_columns(
    connection: &rusqlite::Connection,
) -> Result<()> {
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
    if missing_kind || missing_user {
        connection
            .execute(
                "UPDATE messages
                 SET overview_kind = CASE
                         WHEN instr(value_json, '\"status\":\"error\"') > 0
                           OR instr(value_json, '\"decision\":\"interrupted\"') > 0
                           OR instr(value_json, '\"decision\":\"canceled\"') > 0
                           OR instr(value_json, '\"decision\":\"rejected\"') > 0
                           OR instr(value_json, '\"state\":\"interrupted\"') > 0
                           OR instr(value_json, '\"state\":\"canceled\"') > 0
                         THEN 3
                         WHEN instr(value_json, '\"type\":\"command\"') > 0 THEN 1
                         WHEN instr(value_json, '\"type\":\"diff\"') > 0
                           OR instr(value_json, '\"type\":\"fileChanges\"') > 0
                         THEN 2
                         ELSE 0
                     END,
                     is_user = instr(value_json, '\"author\":\"you\"') > 0",
                [],
            )
            .context("failed to backfill SQLite transcript overview metadata")?;
    }
    Ok(())
}

fn backfill_missing_sqlite_session_overviews(
    connection: &rusqlite::Connection,
) -> Result<()> {
    let sessions = {
        let mut statement = connection
            .prepare(
                "SELECT session.id, session.value_json
                 FROM sessions AS session
                 WHERE EXISTS (
                     SELECT 1
                     FROM messages AS message
                     WHERE message.session_id = session.id
                 )
                   AND NOT EXISTS (
                     SELECT 1
                     FROM session_overviews AS overview
                     WHERE overview.session_id = session.id
                 )
                 ORDER BY session.id",
            )
            .context("failed to prepare missing transcript overview backfill")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("failed to query missing transcript overview backfill")?;
        let mut sessions = Vec::new();
        let mut skipped_rows = 0_usize;
        for row in rows {
            match row {
                Ok(session) => sessions.push(session),
                Err(err) => {
                    skipped_rows += 1;
                    eprintln!(
                        "persist> skipping unreadable transcript overview candidate: {err:#}"
                    );
                }
            }
        }
        if skipped_rows > 0 {
            eprintln!(
                "persist> skipped {skipped_rows} unreadable transcript overview candidate(s)"
            );
        }
        sessions
    };
    let mut load_messages = connection
        .prepare(
            "SELECT position, overview_kind, is_user
             FROM messages
             WHERE session_id = ?1
             ORDER BY position",
        )
        .context("failed to prepare transcript overview metadata backfill")?;
    let mut insert = connection
        .prepare(
            "INSERT INTO session_overviews(session_id, value_blob)
             VALUES(?1, ?2)",
        )
        .context("failed to prepare transcript overview blob backfill")?;
    let mut skipped_sessions = 0_usize;
    for (session_id, session_value_json) in sessions {
        let is_remote_proxy =
            match PersistedRemoteProxyIdentity::from_session_json(&session_value_json)
                .and_then(|identity| identity.is_remote_proxy())
            {
                Ok(is_remote_proxy) => is_remote_proxy,
                Err(err) => {
                    skipped_sessions += 1;
                    eprintln!(
                        "persist> skipping transcript overview backfill for invalid session \
                         `{session_id}`: {err:#}"
                    );
                    continue;
                }
            };
        if is_remote_proxy {
            // Remote proxies persist only a bounded suffix at nonzero
            // positions and never own a local overview blob.
            continue;
        }

        let value_blob =
            match build_sqlite_session_overview_blob(&mut load_messages, &session_id) {
                Ok(value_blob) => value_blob,
                Err(err) => {
                    skipped_sessions += 1;
                    eprintln!(
                        "persist> skipping transcript overview backfill for invalid session \
                         `{session_id}`: {err:#}"
                    );
                    continue;
                }
            };
        insert
            .execute(rusqlite::params![session_id, value_blob])
            .with_context(|| {
                format!("failed to backfill transcript overview for `{session_id}`")
            })?;
    }
    if skipped_sessions > 0 {
        eprintln!(
            "persist> skipped {skipped_sessions} invalid transcript overview session(s)"
        );
    }
    Ok(())
}

fn build_sqlite_session_overview_blob(
    load_messages: &mut rusqlite::Statement<'_>,
    session_id: &str,
) -> Result<Vec<u8>> {
    let rows = load_messages
        .query_map(rusqlite::params![session_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })
        .with_context(|| {
            format!("failed to query transcript overview metadata for `{session_id}`")
        })?;
    let mut value_blob = Vec::new();
    for row in rows {
        let (position, kind_index, is_user) = row.with_context(|| {
            format!("failed to read transcript overview metadata for `{session_id}`")
        })?;
        let position = usize::try_from(position)
            .context("transcript overview position is negative or too large")?;
        if position != value_blob.len() {
            bail!(
                "transcript overview for `{session_id}` has a gap at position {}",
                value_blob.len()
            );
        }
        let kind = match kind_index {
            0 => ConversationOverviewKind::Text,
            1 => ConversationOverviewKind::Command,
            2 => ConversationOverviewKind::Diff,
            3 => ConversationOverviewKind::Error,
            _ => bail!("transcript overview for `{session_id}` has invalid kind {kind_index}"),
        };
        value_blob.push(encode_conversation_overview_message(kind, is_user));
    }
    Ok(value_blob)
}
