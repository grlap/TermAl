// A history page combines a resident tail with a durable prefix. Pin the
// SQLite view at the resident capture, so rollback cannot mix two transcripts.

const SESSION_HISTORY_CAPTURE_TIMEOUT: Duration = Duration::from_secs(60);
const SESSION_HISTORY_WARNING_AFTER: Duration = Duration::from_secs(10);
const SESSION_HISTORY_RETRY_DELAY: Duration = Duration::from_millis(25);

fn history_snapshot_is_busy(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(code, _)
        if code.code == rusqlite::ErrorCode::DatabaseBusy)
}

fn observe_history_capture_budget(
    spent: Duration,
    warned: &mut bool,
    warn: &mut impl FnMut(Duration),
    last_failure: &str,
) -> Result<(), ApiError> {
    if !*warned && spent >= SESSION_HISTORY_WARNING_AFTER {
        *warned = true;
        warn(spent);
    }
    if spent >= SESSION_HISTORY_CAPTURE_TIMEOUT {
        return Err(ApiError::internal(format!(
            "failed to capture session history snapshot before its deadline: {last_failure}"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct SessionHistorySelector<'a> {
    before: Option<&'a str>,
    after: Option<&'a str>,
    around: Option<usize>,
    start: Option<usize>,
    from_start: bool,
    limit: usize,
}

impl SessionHistorySelector<'_> {
    fn cursor(&self) -> Option<&str> {
        self.before.or(self.after)
    }

    fn range(&self, cursor: Option<usize>, count: usize) -> Result<(usize, usize), ApiError> {
        let range = if let Some(start) = self.start {
            if start >= count && count > 0 {
                return Err(ApiError::conflict(
                    "session history position is beyond the current transcript; refresh the session overview",
                ));
            }
            let start = start.min(count);
            (start, start.saturating_add(self.limit).min(count))
        } else if let Some(around) = self.around {
            if around >= count && count > 0 {
                return Err(ApiError::conflict(
                    "session history position is beyond the current transcript; refresh the session overview",
                ));
            }
            let end = around
                .saturating_sub(self.limit / 2)
                .saturating_add(self.limit)
                .min(count);
            (end.saturating_sub(self.limit), end)
        } else if self.from_start {
            (0, self.limit.min(count))
        } else if self.after.is_some() {
            let start = cursor.unwrap_or(count).saturating_add(1).min(count);
            (start, start.saturating_add(self.limit).min(count))
        } else {
            let end = cursor.unwrap_or(count);
            (end.saturating_sub(self.limit), end)
        };
        if range.1 > count {
            return Err(ApiError::conflict(
                "session history cursor is beyond the current transcript; refresh the session tail",
            ));
        }
        Ok(range)
    }
}

struct LocalSessionHistorySnapshot {
    messages: MessageSequence,
    start_index: usize,
    cursor_position: Option<usize>,
    message_count: usize,
    revision: u64,
    session_mutation_stamp: u64,
    body_seq: Option<u64>,
    body_seq_epoch: Option<String>,
    persistence: Option<rusqlite::Connection>,
}

fn missing_history_cursor() -> ApiError {
    ApiError::conflict("session history cursor is no longer available; refresh the session tail")
}

impl AppState {
    fn capture_local_session_history(
        &self,
        session_id: &str,
        selector: SessionHistorySelector<'_>,
    ) -> Result<LocalSessionHistorySnapshot, ApiError> {
        let started = std::time::Instant::now();
        self.capture_local_session_history_with_wait(
            session_id,
            selector,
            || started.elapsed(),
            std::thread::sleep,
            |elapsed| {
                eprintln!(
                    "history snapshot warning> still waiting after {} ms",
                    elapsed.as_millis()
                );
            },
            || open_sqlite_history_snapshot_with_busy_timeout(
                self.persistence_path.as_ref(), Duration::ZERO,
            ),
        )
    }

    // Keep the clock, diagnostic and off-lock backoff in this capture owner.
    // The opening boundary permits a prepared read connection in contention
    // tests; production always opens the existing history snapshot helper.
    fn capture_local_session_history_with_wait(
        &self,
        session_id: &str,
        selector: SessionHistorySelector<'_>,
        mut elapsed: impl FnMut() -> Duration,
        mut pause: impl FnMut(Duration),
        mut warn: impl FnMut(Duration),
        mut open: impl FnMut() -> Result<rusqlite::Connection>,
    ) -> Result<LocalSessionHistorySnapshot, ApiError> {
        let mut persistence = None;
        let mut wait_pending = false;
        let mut warned = false;
        let mut last_failure = "history state lock is busy".to_owned();
        loop {
            let spent = elapsed();
            observe_history_capture_budget(spent, &mut warned, &mut warn, &last_failure)?;
            if wait_pending {
                let remaining = SESSION_HISTORY_CAPTURE_TIMEOUT.saturating_sub(spent);
                let until_warning = if warned {
                    remaining
                } else {
                    SESSION_HISTORY_WARNING_AFTER.saturating_sub(spent)
                };
                pause(SESSION_HISTORY_RETRY_DELAY.min(remaining).min(until_warning));
                wait_pending = false;
                continue;
            }
            let Some(inner) = self.inner.try_lock_now() else {
                // No SQLite transaction or global guard survives a backoff.
                drop(persistence.take());
                last_failure = "history state lock is busy".to_owned();
                wait_pending = true;
                continue;
            };
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(ApiError::local_session_missing)?;
            let record = &inner.sessions[index];
            let cursor_position = selector.cursor().and_then(|id| {
                record
                    .message_positions
                    .get(id)
                    .copied()
                    .map(|local_index| global_message_index(record, local_index))
            });
            let message_count =
                usize::try_from(session_message_count(record)).unwrap_or(usize::MAX);
            let needs_persistence = if selector.cursor().is_some() && cursor_position.is_none() {
                // A fully resident rollback may have removed this cursor while
                // SQLite still contains it. Never resurrect it from the old DB.
                if record.message_start_index == 0 {
                    return Err(missing_history_cursor());
                }
                true
            } else {
                selector.range(cursor_position, message_count)?.0 < record.message_start_index
            };
            if needs_persistence && persistence.is_none() {
                drop(inner);
                // Reader setup and any backoff stay outside the global lock.
                // busy=0 is selected before setup, not after opening returns.
                match open() {
                    Ok(connection) => persistence = Some(connection),
                    Err(err) => {
                        let contended = err.chain().any(|cause| {
                            cause.downcast_ref::<rusqlite::Error>()
                                .is_some_and(history_snapshot_is_busy)
                        });
                        let detail = format!("failed to open session history snapshot: {err:#}");
                        if !contended {
                            return Err(ApiError::internal(detail));
                        }
                        last_failure = detail;
                        wait_pending = true;
                    }
                }
                continue;
            }
            if needs_persistence {
                // BEGIN DEFERRED alone does not establish a read snapshot.
                // This indexed existence read pins it without loading bodies.
                // Eviction follows durability; rollback retains the whole new
                // transcript until its write lands. Thus the nonresident prefix
                // in this snapshot belongs to the resident capture below.
                let pin: rusqlite::Result<bool> = persistence
                    .as_ref()
                    .expect("history reader should be open")
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id = ?1)",
                        [session_id],
                        |row| row.get(0),
                    );
                if let Err(err) = pin {
                    // An external BUSY can clear after another connection
                    // finishes. LOCKED may be same-connection misuse and must
                    // not be hidden by retrying it.
                    drop(inner);
                    drop(persistence.take());
                    let contended = history_snapshot_is_busy(&err);
                    let detail = format!("failed to pin session history snapshot: {err:#}");
                    if !contended {
                        return Err(ApiError::internal(detail));
                    }
                    last_failure = detail;
                    wait_pending = true;
                    continue;
                }
            }
            let snapshot = LocalSessionHistorySnapshot {
                messages: record.session.messages.clone(),
                start_index: record.message_start_index,
                cursor_position,
                message_count,
                revision: inner.revision,
                session_mutation_stamp: record.mutation_stamp,
                body_seq: record.wire_body_seq(),
                body_seq_epoch: record.wire_body_seq_epoch(&self.server_instance_id),
                persistence,
            };
            drop(inner);
            // Account for pinning and resident capture as well as retries.
            // Diagnostics never perform I/O while holding StateInner.
            observe_history_capture_budget(
                elapsed(), &mut warned, &mut warn, "history capture completed after its budget",
            )?;
            return Ok(snapshot);
        }
    }
}

impl LocalSessionHistorySnapshot {
    fn page(
        self,
        session_id: &str,
        selector: SessionHistorySelector<'_>,
        server_instance_id: String,
    ) -> Result<SessionHistoryResponse, ApiError> {
        let cursor_position = match (selector.cursor(), self.cursor_position) {
            (_, Some(position)) => Some(position),
            (Some(cursor), None) => {
                let position = persisted_message_position_with_connection(
                    self.persistence
                        .as_ref()
                        .expect("cold cursor needs a pinned reader"),
                    session_id,
                    cursor,
                )
                .map_err(|err| {
                    ApiError::internal(format!("failed to resolve session history cursor: {err:#}"))
                })?
                .filter(|position| *position < self.start_index)
                .ok_or_else(missing_history_cursor)?;
                Some(position)
            }
            (None, None) => None,
        };
        let (start_index, end_index) = selector.range(cursor_position, self.message_count)?;
        let mut page = vec![None; end_index.saturating_sub(start_index)];
        let resident_start = start_index
            .saturating_sub(self.start_index)
            .min(self.messages.len());
        let resident_end = end_index
            .saturating_sub(self.start_index)
            .min(self.messages.len());
        for (offset, message) in self.messages.window(resident_start..resident_end).into_iter().enumerate() {
            let global_index = self.start_index + resident_start + offset;
            page[global_index - start_index] = Some(message);
        }
        if page.iter().any(Option::is_none) {
            let persisted = load_persisted_message_range_with_connection(
                self.persistence
                    .as_ref()
                    .expect("cold page needs a pinned reader"),
                session_id,
                start_index,
                end_index,
            )
            .map_err(|err| {
                ApiError::internal(format!("failed to load session history page: {err:#}"))
            })?;
            for (position, message) in persisted {
                if position >= start_index && position < end_index {
                    let slot = &mut page[position - start_index];
                    if slot.is_none() {
                        *slot = Some(message);
                    }
                }
            }
        }
        let page = page
            .into_iter()
            .enumerate()
            .map(|(offset, message)| {
                message.ok_or_else(|| ApiError::conflict(format!(
                "session history is missing persisted position {}; refresh the session tail",
                start_index + offset
            )))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = start_index > 0 && !page.is_empty();
        let next_before = has_more
            .then(|| page.first().map(|message| message.id().to_owned()))
            .flatten();
        let has_newer = end_index < self.message_count && !page.is_empty();
        let next_after = has_newer
            .then(|| page.last().map(|message| message.id().to_owned()))
            .flatten();
        Ok(SessionHistoryResponse {
            messages: page,
            next_before,
            has_more,
            next_after,
            has_newer,
            message_start_index: start_index,
            message_count: u32::try_from(self.message_count).unwrap_or(u32::MAX),
            revision: self.revision,
            session_mutation_stamp: self.session_mutation_stamp,
            body_seq: self.body_seq,
            body_seq_epoch: self.body_seq_epoch,
            server_instance_id,
        })
    }
}
