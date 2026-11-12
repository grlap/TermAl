// Split out of persist.rs: owns normalized session loading and quarantine,
// prompt-history reads, retained transcript tails, cursors and pages.
// Does not own connection/snapshot lifetimes, global metadata adoption,
// delegation loading, schema maintenance or persistence transactions.

#[cfg(test)]
fn load_session_records_from_sqlite(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<Vec<PersistedSessionRecord>> {
    load_session_records_from_sqlite_with_skipped(connection, path).map(|(records, _, _)| records)
}

#[cfg(test)]
fn load_session_records_from_sqlite_with_skipped(
    connection: &rusqlite::Connection,
    path: &FsPath,
) -> Result<(Vec<PersistedSessionRecord>, BTreeSet<String>, usize)> {
    load_session_records_from_sqlite_with_skipped_and_timings(
        connection,
        path,
        &mut BootStateLoadTimings::new(false),
    )
}

fn load_session_records_from_sqlite_with_skipped_and_timings(
    connection: &rusqlite::Connection,
    path: &FsPath,
    timings: &mut BootStateLoadTimings,
) -> Result<(Vec<PersistedSessionRecord>, BTreeSet<String>, usize)> {
    let mut statement = connection
        .prepare("SELECT id, value_json FROM sessions ORDER BY rowid")
        .with_context(|| format!("failed to prepare session load from `{}`", path.display()))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .with_context(|| {
            format!(
                "failed to query persisted sessions from `{}`",
                path.display()
            )
        })?;
    let mut records = Vec::new();
    let mut quarantined_ids = BTreeSet::new();
    let mut skipped = 0;
    let mut missing_history_count = 0;
    let mut missing_history_sample = Vec::new();
    for row in rows {
        let (session_id, encoded) = match row {
            Ok(row) => row,
            Err(err) => {
                skipped += 1;
                eprintln!(
                    "persist> skipping unreadable session row from `{}`: {err:#}",
                    path.display()
                );
                continue;
            }
        };
        let loaded_record = (|| -> Result<(PersistedSessionRecord, bool)> {
            let mut record = {
                let _decode_timing = timings.scoped(BootStateLoadPhase::SessionDecode);
                let mut record: PersistedSessionRecord = serde_json::from_str(&encoded)
                    .with_context(|| format!("failed to parse persisted session `{session_id}`"))?;
                if record.session.id != session_id {
                    bail!(
                        "row id `{session_id}` does not match embedded id `{}`",
                        record.session.id
                    );
                }
                normalize_persisted_session_prompt_history(&mut record.session);
                validate_persisted_session_fields(
                    &record.session,
                    record.external_session_id.as_deref(),
                )
                .with_context(|| format!("persisted session `{session_id}` failed validation"))?;
                validate_remote_proxy_identity(
                    record.remote_id.as_deref(),
                    record.remote_session_id.as_deref(),
                )
                .with_context(|| {
                    format!("persisted session `{session_id}` has invalid remote proxy identity")
                })?;
                record
            };
            let started = timings.start();
            let tail = load_persisted_session_tail(connection, path, &mut record);
            timings.finish(BootStateLoadPhase::TranscriptTails, started);
            tail?;
            let started = timings.start();
            let history = load_persisted_prompt_history(connection, path, &mut record);
            timings.finish(BootStateLoadPhase::PromptHistories, started);
            let missing_history = history?;
            Ok((record, missing_history))
        })();
        match loaded_record {
            Ok((record, missing_history)) => {
                if missing_history {
                    missing_history_count += 1;
                    if missing_history_sample.len() < 5 {
                        missing_history_sample.push(session_id);
                    }
                }
                records.push(record);
            }
            Err(err) => {
                skipped += 1;
                quarantined_ids.insert(session_id.clone());
                eprintln!("persist> skipping invalid session `{session_id}`: {err:#}");
            }
        }
    }
    if missing_history_count > 0 {
        // Delta persistence need not write an empty history row: empty/oversized
        // prompts, non-Text messages and partial-window updates can leave
        // its independent mutation stamp unchanged. Report one neutral summary
        // for missing history on nonempty transcripts, without classifying or
        // scanning their bodies. Never reconstruct composer history from text.
        if missing_history_count > missing_history_sample.len() {
            missing_history_sample.push("…".to_owned());
        }
        eprintln!(
            "persist> history info: loaded empty composer history for {missing_history_count} session(s) with persisted messages and no normalized history row; sessions=[{}], store=`{}`; this can be normal when no eligible composer prompts were recorded (for example empty or oversized prompts, non-text messages or partial-window updates); transcript contents are not composer-history authority",
            missing_history_sample.join(", "),
            path.display()
        );
    }
    Ok((records, quarantined_ids, skipped))
}

fn load_persisted_prompt_history(
    connection: &rusqlite::Connection,
    path: &FsPath,
    record: &mut PersistedSessionRecord,
) -> Result<bool> {
    let stored_history = connection
        .query_row(
            "SELECT value_json
             FROM session_prompt_histories
             WHERE session_id = ?1",
            rusqlite::params![record.session.id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .with_context(|| {
            format!(
                "failed to load persisted prompt history for `{}` from `{}`",
                record.session.id,
                path.display()
            )
        })?;
    let mut missing_history_with_messages = false;
    record.session.prompt_history = match stored_history {
        Some(encoded) => normalize_prompt_history(
            serde_json::from_str::<Vec<String>>(&encoded).with_context(|| {
                format!(
                    "failed to parse persisted prompt history for `{}`",
                    record.session.id
                )
            })?,
        ),
        None => {
            // A single indexed existence probe includes rows older than the
            // loaded tail without reading bodies or retired author metadata.
            // Present history rows skip this probe entirely.
            let has_messages: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id = ?1)",
                    rusqlite::params![record.session.id],
                    |row| row.get(0),
                )
                .with_context(|| {
                    format!(
                        "failed to inspect persisted transcript presence for `{}`",
                        record.session.id
                    )
                })?;
            missing_history_with_messages = has_messages;
            Vec::new()
        }
    };
    Ok(missing_history_with_messages)
}

fn load_persisted_session_tail(
    connection: &rusqlite::Connection,
    path: &FsPath,
    record: &mut PersistedSessionRecord,
) -> Result<()> {
    let total_message_count = usize::try_from(record.session.message_count)
        .context("persisted transcript count does not fit this platform")?;
    let start_index = total_message_count.saturating_sub(SQLITE_SESSION_TAIL_MESSAGES);
    let mut statement = connection
        .prepare(
            "SELECT position, message_id, value_json
             FROM messages
             WHERE session_id = ?1 AND position >= ?2 AND position < ?3
             ORDER BY position ASC",
        )
        .with_context(|| {
            format!(
                "failed to prepare persisted transcript tail for `{}`",
                record.session.id
            )
        })?;
    let rows = statement
        .query_map(
            rusqlite::params![
                record.session.id,
                i64::try_from(start_index)
                    .context("persisted transcript position exceeds SQLite integer range")?,
                i64::try_from(total_message_count)
                    .context("persisted transcript count exceeds SQLite integer range")?
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .with_context(|| {
            format!(
                "failed to query persisted transcript tail for `{}`",
                record.session.id
            )
        })?;
    let mut messages = Vec::new();
    let mut unusable_tail: Option<String> = None;
    for (expected_position, row) in (start_index..total_message_count).zip(rows) {
        let (position, message_id, encoded) = row.with_context(|| {
            format!(
                "failed to read persisted transcript tail for `{}` from `{}`",
                record.session.id,
                path.display()
            )
        })?;
        let actual_position = usize::try_from(position)
            .context("persisted transcript position is negative or too large")?;
        if actual_position != expected_position {
            unusable_tail = Some(format!("gap at position {expected_position}"));
            break;
        }
        let message: Message = serde_json::from_str(&encoded).with_context(|| {
            format!(
                "failed to parse transcript position {actual_position} for `{}`",
                record.session.id
            )
        })?;
        if message.id() != message_id {
            bail!(
                "transcript position {actual_position} row id `{message_id}` does not match embedded id `{}` for `{}`",
                message.id(),
                record.session.id
            );
        }
        messages.push(message);
    }
    let expected_tail_len = total_message_count.saturating_sub(start_index);
    if unusable_tail.is_none() && messages.len() != expected_tail_len {
        unusable_tail = Some(format!(
            "expected {expected_tail_len} retained messages but loaded {}",
            messages.len()
        ));
    }
    // A REMOTE-PROXY transcript whose local rows do not cover the range
    // implied by `message_count` is a hydration state, not corruption: the
    // transcript lives on the remote host, so its metadata can legitimately
    // know a count while zero `messages` rows exist locally.
    //
    // A LOCAL session in the same shape must be quarantined instead: its
    // normalized SQLite rows are the transcript authority, not a cache that
    // remote hydration can replace. Return before mutating record so the
    // row-level loader quarantines the session and full persistence preserves
    // the damaged rows for deliberate recovery instead of overwriting them.
    if let Some(reason) = unusable_tail {
        let is_remote_proxy = validate_remote_proxy_identity(
            record.remote_id.as_deref(),
            record.remote_session_id.as_deref(),
        )?
        .is_some();
        if !is_remote_proxy {
            bail!(
                "local session `{}` transcript tail is inconsistent ({reason}); preserving its persisted row for recovery",
                record.session.id
            );
        }
        if !messages.is_empty() {
            // A partial tail means the rows themselves disagree with the
            // metadata, which is worth surfacing. Zero rows is the ordinary
            // unhydrated/proxy case and stays quiet to avoid boot-time noise
            // proportional to the number of proxy sessions.
            eprintln!(
                "persist> session `{}` transcript tail is inconsistent ({reason}); starting it unhydrated",
                record.session.id
            );
        }
        record.message_start_index = total_message_count;
        record.session.messages = MessageSequence::default();
        record.session.messages_loaded = total_message_count == 0;
        return Ok(());
    }
    record.message_start_index = start_index;
    record.session.messages = messages.into();
    record.session.messages_loaded = start_index == 0;
    Ok(())
}

fn persisted_message_position(
    path: &FsPath,
    session_id: &str,
    message_id: &str,
) -> Result<Option<usize>> {
    let connection = open_sqlite_state_read_connection(path)?;
    persisted_message_position_with_connection(&connection, session_id, message_id)
}

fn persisted_message_position_with_connection(
    connection: &rusqlite::Connection,
    session_id: &str,
    message_id: &str,
) -> Result<Option<usize>> {
    match connection.query_row(
        "SELECT position
         FROM messages
         WHERE session_id = ?1 AND message_id = ?2",
        rusqlite::params![session_id, message_id],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(position) => Ok(Some(usize::try_from(position).context(
            "persisted transcript cursor position is negative or too large",
        )?)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err).with_context(|| {
            format!("failed to resolve transcript cursor for session `{session_id}`")
        }),
    }
}

#[cfg(test)]
fn load_persisted_message_range(
    path: &FsPath,
    session_id: &str,
    start_index: usize,
    end_index: usize,
) -> Result<Vec<(usize, Message)>> {
    if start_index >= end_index {
        return Ok(Vec::new());
    }
    let connection = open_sqlite_state_read_connection(path)?;
    load_persisted_message_range_with_connection(&connection, session_id, start_index, end_index)
}

fn load_persisted_message_range_with_connection(
    connection: &rusqlite::Connection,
    session_id: &str,
    start_index: usize,
    end_index: usize,
) -> Result<Vec<(usize, Message)>> {
    if start_index >= end_index {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(
            "SELECT position, message_id, value_json
             FROM messages
             WHERE session_id = ?1 AND position >= ?2 AND position < ?3
             ORDER BY position ASC",
        )
        .with_context(|| format!("failed to prepare transcript page for session `{session_id}`"))?;
    let rows = statement
        .query_map(
            rusqlite::params![
                session_id,
                i64::try_from(start_index)
                    .context("transcript page start exceeds SQLite integer range")?,
                i64::try_from(end_index)
                    .context("transcript page end exceeds SQLite integer range")?,
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .with_context(|| format!("failed to query transcript page for session `{session_id}`"))?;
    let mut messages = Vec::new();
    for row in rows {
        let (position, message_id, encoded) = row.with_context(|| {
            format!("failed to read transcript page for session `{session_id}`")
        })?;
        let position = usize::try_from(position)
            .context("persisted transcript position is negative or too large")?;
        let message: Message = serde_json::from_str(&encoded).with_context(|| {
            format!("failed to parse transcript position {position} for session `{session_id}`")
        })?;
        if message.id() != message_id {
            bail!(
                "transcript position {position} row id `{message_id}` does not match embedded id `{}` for session `{session_id}`",
                message.id()
            );
        }
        messages.push((position, message));
    }
    Ok(messages)
}
