// Split out of persist.rs: owns normalized session/message serialization,
// invalid-session isolation, session/history/overview writes and retained-row
// deletion that preserves quarantined sessions. Does not own transaction
// boundaries, writer admission, global metadata/delegations or the differential
// message-row writer in persist_sqlite_messages.rs.

struct SerializedPersistedMessage {
    position: usize,
    message_id: String,
    value_json: String,
    overview_kind: ConversationOverviewKind,
    is_user: bool,
}

struct SerializedPersistedSession {
    session_id: String,
    message_start_index: usize,
    message_count: usize,
    write_overview: bool,
    prompt_history_value_json: Option<String>,
    value_json: String,
    messages: Vec<SerializedPersistedMessage>,
}

fn serialize_persisted_session(
    record: &PersistedSessionRecord,
) -> Result<SerializedPersistedSession> {
    let remote_proxy_identity = validate_remote_proxy_identity(
        record.remote_id.as_deref(),
        record.remote_session_id.as_deref(),
    )
    .with_context(|| {
        format!(
            "persisted session `{}` has invalid remote proxy identity",
            record.session.id
        )
    })?;
    let mut metadata = record.clone();
    let retained_end = record
        .message_start_index
        .checked_add(record.session.messages.len())
        .context("persisted transcript position overflow")?;
    let total_message_count = if record.session.messages_loaded {
        retained_end
    } else {
        retained_end.max(
            usize::try_from(record.session.message_count)
                .context("persisted transcript count does not fit this platform")?,
        )
    };
    let prompt_history_value_json = record
        .persist_prompt_history
        .then(|| {
            serde_json::to_string(&record.session.prompt_history)
                .context("failed to serialize persisted prompt history")
        })
        .transpose()?;
    metadata.session.messages.clear();
    // Prompt history has an independent mutation watermark and SQLite row. It
    // must not inflate the metadata JSON rewritten by every streaming commit.
    metadata.session.prompt_history.clear();
    metadata.session.message_count =
        u32::try_from(total_message_count).context("persisted transcript exceeds wire limit")?;
    metadata.session.messages_loaded = total_message_count == 0;
    metadata.message_start_index = 0;

    let value_json = serde_json::to_string(&metadata)
        .context("failed to serialize persisted session metadata")?;
    let messages = record
        .session
        .messages
        .iter()
        .enumerate()
        .map(|(local_index, message)| {
            let position = record
                .message_start_index
                .checked_add(local_index)
                .context("persisted transcript position overflow")?;
            let value_json = serde_json::to_string(message)
                .context("failed to serialize persisted transcript message")?;
            let (overview_kind, is_user) = conversation_overview_message_metadata(message);
            Ok(SerializedPersistedMessage {
                position,
                message_id: message.id().to_owned(),
                value_json,
                overview_kind,
                is_user,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(SerializedPersistedSession {
        session_id: record.session.id.clone(),
        message_start_index: record.message_start_index,
        message_count: total_message_count,
        write_overview: remote_proxy_identity.is_none(),
        prompt_history_value_json,
        value_json,
        messages,
    })
}

fn serialize_persisted_sessions_with_isolation(
    sessions: &[PersistedSessionRecord],
) -> Vec<SerializedPersistedSession> {
    let mut serialized_sessions = Vec::with_capacity(sessions.len());
    let mut skipped = 0_usize;
    for record in sessions {
        match serialize_persisted_session(record) {
            Ok(session) => serialized_sessions.push(session),
            Err(err) => {
                skipped += 1;
                eprintln!(
                    "persist> preserving the last good row for invalid in-memory session `{}`: {err:#}",
                    record.session.id
                );
            }
        }
    }
    if skipped > 0 {
        eprintln!(
            "persist> skipped {skipped} invalid in-memory session record(s) while persisting"
        );
    }
    serialized_sessions
}

fn write_serialized_persisted_session(
    tx: &rusqlite::Transaction<'_>,
    session: &SerializedPersistedSession,
) -> Result<()> {
    tx.execute(
        "INSERT INTO sessions(id, value_json) VALUES(?1, ?2)
         ON CONFLICT(id) DO UPDATE SET value_json = excluded.value_json",
        rusqlite::params![session.session_id, session.value_json],
    )
    .with_context(|| {
        format!(
            "failed to write persisted session metadata for `{}`",
            session.session_id
        )
    })?;
    if let Some(prompt_history_value_json) = &session.prompt_history_value_json {
        tx.execute(
            "INSERT INTO session_prompt_histories(session_id, value_json)
             VALUES(?1, ?2)
             ON CONFLICT(session_id) DO UPDATE SET value_json = excluded.value_json",
            rusqlite::params![session.session_id, prompt_history_value_json],
        )
        .with_context(|| {
            format!(
                "failed to write persisted prompt history for `{}`",
                session.session_id
            )
        })?;
    }
    write_serialized_persisted_messages(tx, session)?;
    if !session.write_overview {
        tx.execute(
            "DELETE FROM session_overviews WHERE session_id = ?1",
            rusqlite::params![session.session_id],
        )
        .with_context(|| {
            format!(
                "failed to clear local transcript overview for remote proxy `{}`",
                session.session_id
            )
        })?;
        return Ok(());
    }
    let mut overview_blob = if session.message_start_index == 0 {
        Vec::with_capacity(session.message_count)
    } else {
        let existing = match tx.query_row(
            "SELECT value_blob
             FROM session_overviews
             WHERE session_id = ?1",
            rusqlite::params![session.session_id],
            |row| row.get::<_, Vec<u8>>(0),
        ) {
            Ok(existing) => existing,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                let mut statement = tx
                    .prepare(
                        "SELECT overview_kind, is_user
                         FROM messages
                         WHERE session_id = ?1 AND position < ?2
                         ORDER BY position",
                    )
                    .context("failed to prepare transcript overview prefix recovery")?;
                statement
                    .query_map(
                        rusqlite::params![
                            session.session_id,
                            i64::try_from(session.message_start_index).context(
                                "persisted transcript position exceeds SQLite integer range"
                            )?,
                        ],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
                    )
                    .context("failed to query transcript overview prefix recovery")?
                    .map(|row| {
                        let (kind_index, is_user) =
                            row.context("failed to read transcript overview prefix")?;
                        let kind = match kind_index {
                            0 => ConversationOverviewKind::Text,
                            1 => ConversationOverviewKind::Command,
                            2 => ConversationOverviewKind::Diff,
                            3 => ConversationOverviewKind::Error,
                            _ => bail!("invalid persisted transcript overview kind {kind_index}"),
                        };
                        Ok(encode_conversation_overview_message(kind, is_user))
                    })
                    .collect::<Result<Vec<_>>>()?
            }
            Err(err) => {
                return Err(err).context("failed to load persisted transcript overview prefix");
            }
        };
        if existing.len() < session.message_start_index {
            bail!(
                "persisted transcript overview for `{}` has {} positions but tail starts at {}",
                session.session_id,
                existing.len(),
                session.message_start_index
            );
        }
        let mut prefix = existing;
        prefix.truncate(session.message_start_index);
        prefix.reserve(session.messages.len());
        prefix
    };
    overview_blob.extend(session.messages.iter().map(|message| {
        encode_conversation_overview_message(message.overview_kind, message.is_user)
    }));
    if overview_blob.len() != session.message_count {
        bail!(
            "persisted transcript overview for `{}` has {} positions but metadata expects {}",
            session.session_id,
            overview_blob.len(),
            session.message_count
        );
    }
    tx.execute(
        "INSERT INTO session_overviews(session_id, value_blob)
         VALUES(?1, ?2)
         ON CONFLICT(session_id) DO UPDATE SET value_blob = excluded.value_blob
         WHERE session_overviews.value_blob IS NOT excluded.value_blob",
        rusqlite::params![session.session_id, overview_blob],
    )
    .with_context(|| {
        format!(
            "failed to write persisted transcript overview for `{}`",
            session.session_id
        )
    })?;
    Ok(())
}

fn remove_missing_persisted_sessions(
    tx: &rusqlite::Transaction<'_>,
    retained_session_ids: &HashSet<&str>,
    quarantined_session_ids: &BTreeSet<String>,
) -> Result<()> {
    let stored_session_ids = {
        let mut statement = tx
            .prepare("SELECT id FROM sessions")
            .context("failed to prepare stored-session replacement")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .context("failed to query stored sessions for replacement")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to read stored sessions for replacement")?
    };
    for session_id in stored_session_ids {
        if retained_session_ids.contains(session_id.as_str())
            || quarantined_session_ids.contains(&session_id)
        {
            continue;
        }
        tx.execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
        )
        .context("failed to remove stale persisted session")?;
    }
    Ok(())
}
