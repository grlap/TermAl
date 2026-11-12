// Split out of persist.rs: owns normalized session/message serialization,
// invalid-session isolation, session/history writes and retained-row
// deletion that preserves quarantined sessions. Does not own transaction
// boundaries, writer admission, global metadata/delegations or the differential
// message-row writer in persist_sqlite_messages.rs.

struct SerializedPersistedMessage {
    position: usize,
    message_id: String,
    value_json: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistSessionRejection {
    session_id: String,
    error: String,
}

impl PersistSessionRejection {
    fn new(session_id: &str, error: &anyhow::Error) -> Self {
        Self {
            session_id: session_id.to_owned(),
            error: format!("{error:#}"),
        }
    }

    fn bounded_observation(&self) -> Self {
        Self {
            session_id: self.session_id.chars().take(128).collect(),
            error: self.error.chars().take(1024).collect(),
        }
    }
}

struct SerializedPersistedSession {
    session_id: String,
    // Remote-proxy transcript IDs belong to the peer, never our allocators.
    local_identity_namespace: bool,
    message_start_index: usize,
    // Logical coverage is independent of the sparse upsert list.
    truncate_to: Option<usize>,
    snapshot: MessageSequence,
    prompt_history_value_json: Option<String>,
    value_json: String,
    messages: Vec<SerializedPersistedMessage>,
}

#[cfg(test)]
thread_local! {
    static PERSIST_MESSAGE_SERIALIZE_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce(&PersistedSessionRecord)>>> =
        std::cell::RefCell::new(None);
}

fn serialize_persisted_session(
    record: &PersistedSessionRecord,
) -> Result<SerializedPersistedSession> {
    serialize_persisted_session_since(record, None)
}

fn serialize_persisted_session_since(
    record: &PersistedSessionRecord,
    baseline: Option<&PersistTranscriptBaseline>,
) -> Result<SerializedPersistedSession> {
    #[cfg(test)]
    if let Some(hook) = PERSIST_MESSAGE_SERIALIZE_HOOK.with(|slot| slot.take()) {
        hook(record);
    }
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
    // Validate the complete captured window, including bodies omitted by the
    // differential baseline. A sparse write cannot prove identity uniqueness.
    let mut identities = HashSet::new();
    for message in &record.session.messages {
        if !identities.insert(message.id()) {
            bail!(
                "persisted session `{}` has duplicate transcript identity",
                record.session.id
            );
        }
    }
    let mut metadata = record.clone_without_messages();
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
        .filter(|(index, _)| {
            let absolute = record.message_start_index + index;
            !baseline.is_some_and(|old| {
                absolute.checked_sub(old.start).is_some_and(|offset| {
                    old.identities.get(offset).is_some_and(|saved| {
                        record.session.messages.matches_identity(*index, saved)
                    })
                })
            })
        })
        .map(|(local_index, message)| {
            let position = record
                .message_start_index
                .checked_add(local_index)
                .context("persisted transcript position overflow")?;
            // Projection is outside StateInner and only visits selected bodies.
            let mut projected;
            let message = if matches!(message, Message::EngramControl { .. }) {
                projected = message.clone();
                if let Message::EngramControl { card, .. } = &mut projected
                    && let Some(cause) = &mut card.causal_failure
                {
                    cause.publication_pending = false;
                }
                &projected
            } else {
                message
            };
            let value_json = serde_json::to_string(message)
                .context("failed to serialize persisted transcript message")?;
            Ok(SerializedPersistedMessage {
                position,
                message_id: message.id().to_owned(),
                value_json,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(SerializedPersistedSession {
        session_id: record.session.id.clone(),
        local_identity_namespace: remote_proxy_identity.is_none(),
        message_start_index: record.message_start_index,
        truncate_to: (retained_end == total_message_count).then_some(retained_end),
        snapshot: record.session.messages.clone(),
        prompt_history_value_json,
        value_json,
        messages,
    })
}

fn serialize_persisted_sessions_with_isolation(
    sessions: &[PersistedSessionRecord],
    rejected: &mut Vec<PersistSessionRejection>,
) -> Vec<SerializedPersistedSession> {
    let mut serialized_sessions = Vec::with_capacity(sessions.len());
    let mut skipped = 0_usize;
    for record in sessions {
        match serialize_persisted_session(record) {
            Ok(session) => serialized_sessions.push(session),
            Err(err) => {
                skipped += 1;
                rejected.push(PersistSessionRejection::new(&record.session.id, &err));
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
    Ok(())
}

/// Reject only a transcript identity collision, retaining the complete last
/// good session row, history and messages inside the outer transaction.
fn write_serialized_persisted_session_isolated(
    tx: &rusqlite::Transaction<'_>,
    session: &SerializedPersistedSession,
) -> Result<Option<PersistSessionRejection>> {
    tx.execute_batch("SAVEPOINT transcript_session_write")?;
    match write_serialized_persisted_session(tx, session) {
        Ok(()) => {
            tx.execute_batch("RELEASE transcript_session_write")?;
            Ok(None)
        }
        Err(error) if error.is::<PersistTranscriptIdentityConflict>() => {
            tx.execute_batch(
                "ROLLBACK TO transcript_session_write; RELEASE transcript_session_write",
            )?;
            Ok(Some(PersistSessionRejection::new(
                &session.session_id,
                &error,
            )))
        }
        Err(error) => Err(error),
    }
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
