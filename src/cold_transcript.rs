// Owns which finished delegation children keep no resident transcript, both at
// boot (from a delegation-link prepass, because delegation rows load later)
// and after persistence (from the live delegation inventory), and installing
// a cold child's last window again for operations that read it under the lock.
// Does not own transcript storage, history paging or delegation lifecycle;
// session_history.rs reads the window through its pinned snapshot.

use super::*;

const TERMINAL_DELEGATION_STATUSES: [&str; 3] = ["completed", "failed", "canceled"];

/// The delegation links that decide whether a child's transcript may be cold.
/// A session is a finished child when it is the child of exactly one terminal
/// delegation and the parent of no delegation that may still read its window:
/// an open one, whose recovery, settlement and card updates read it, or a
/// terminal evaluator whose card a submission in progress or a recorded
/// write still to be projected may rewrite.
#[derive(Default)]
pub(crate) struct DelegationLinkCut {
    child_links: HashMap<String, (usize, bool)>,
    parents: HashSet<String>,
}

impl DelegationLinkCut {
    /// Reads only the link fields of the stored delegation rows before the
    /// deferred inventory loads them, decoding just the failed evaluator rows
    /// whose card may still change. A row that is not valid JSON is skipped:
    /// the inventory quarantines it, so it is no link at runtime either.
    pub(crate) fn load(connection: &rusqlite::Connection) -> Result<Self> {
        let mut statement = connection
            .prepare(
                "SELECT json_extract(value_json, '$.childSessionId'),
                        json_extract(value_json, '$.parentSessionId'),
                        json_extract(value_json, '$.status'),
                        CASE WHEN json_extract(value_json, '$.mode') = 'evaluator'
                                  AND json_extract(value_json, '$.status') = 'failed'
                             THEN value_json END
                 FROM delegations
                 WHERE json_valid(value_json)",
            )
            .context("failed to prepare the delegation link prepass")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .context("failed to query the delegation link prepass")?;
        let mut cut = Self::default();
        for row in rows {
            let (child, parent, status, failed_evaluator) =
                row.context("failed to read a delegation link")?;
            let terminal = status
                .as_deref()
                .is_some_and(|status| TERMINAL_DELEGATION_STATUSES.contains(&status));
            // No submission is in progress before boot. A row that does not
            // decode is quarantined by the inventory, so it is no link.
            let card_may_change = match failed_evaluator {
                Some(json) => match serde_json::from_str::<DelegationRecord>(&json) {
                    Ok(record) => acceptance_card_may_still_change(&record, false),
                    Err(_) => continue,
                },
                None => false,
            };
            cut.add(child, parent, terminal, !terminal || card_may_change);
        }
        Ok(cut)
    }

    /// The same links from loaded records, before they are published, when no
    /// submission can be in progress.
    pub(crate) fn from_records<'a>(
        records: impl IntoIterator<Item = &'a DelegationRecord>,
    ) -> Self {
        let mut cut = Self::default();
        for record in records {
            let terminal = delegation_is_terminal(record.status);
            cut.add(
                Some(record.child_session_id.clone()),
                Some(record.parent_session_id.clone()),
                terminal,
                !terminal || acceptance_card_may_still_change(record, false),
            );
        }
        cut
    }

    fn add(
        &mut self,
        child: Option<String>,
        parent: Option<String>,
        terminal: bool,
        reads_parent: bool,
    ) {
        // A row whose child cannot be read is an ambiguous link for nobody in
        // particular; an unreadable parent cannot make any session a parent.
        if let Some(child) = child {
            let link = self.child_links.entry(child).or_insert((0, true));
            link.0 += 1;
            link.1 &= terminal;
        }
        if let Some(parent) = parent.filter(|_| reads_parent) {
            self.parents.insert(parent);
        }
    }

    pub(crate) fn is_finished_child(&self, session_id: &str) -> bool {
        self.child_links.get(session_id) == Some(&(1, true)) && !self.parents.contains(session_id)
    }
}

/// Sessions that own a stored test-run card whose run was still live, so the
/// first scan after a restart may update that card from its transcript: such
/// an owner keeps its tail at boot. An owner whose stored cards are all of
/// terminal runs has nothing running and starts cold; its cards keep their
/// stored terminal flag instead of being re-derived (persist.rs).
pub(crate) fn sessions_with_live_test_run_cards<'a>(
    cards: impl IntoIterator<Item = &'a TestRunCardRef>,
) -> HashSet<String> {
    cards
        .into_iter()
        .filter(|card| !card.terminal)
        .map(|card| card.session_id.clone())
        .collect()
}

fn finished_child_status(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Idle | SessionStatus::Error)
}

/// Boot form: decided from the stored session row and the link prepass.
pub(crate) fn persisted_session_transcript_starts_cold(
    record: &PersistedSessionRecord,
    links: &DelegationLinkCut,
    card_owner_sessions: &HashSet<String>,
) -> bool {
    record.session.parent_delegation_id.is_some()
        && record.remote_id.is_none()
        && finished_child_status(record.session.status)
        && record.queued_prompts.is_empty()
        && record.queued_peer_messages.is_empty()
        && record.engram_unadmitted_active_turn.is_none()
        && record.restart_recovery.is_none()
        && !card_owner_sessions.contains(&record.session.id)
        && links.is_finished_child(&record.session.id)
}

/// What a persistence pass may do with a session's resident transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColdTrimDecision {
    /// A finished child with nothing able to read or amend its window: trim
    /// it to nothing.
    Trim,
    /// A finished child that something transient still holds (an open
    /// file-change grace window, an in-flight capture, a pin, a test-run
    /// card, an unsettled delegation, an evaluator card still to be rewritten
    /// by a submission in progress or a recorded write's projection, or an
    /// unloaded inventory): keep the tail and decide again after a later
    /// persistence pass.
    Retry,
    /// Not a finished child: the ordinary 64-message rule applies.
    Never,
}

/// Process-local state of the cold-transcript rule; never persisted. Both
/// maps hold only ids of sessions that exist when they are inserted, and a
/// recheck drops an id whose session is gone, so they stay bounded by the
/// live session set.
#[derive(Default)]
pub(crate) struct ColdTranscriptState {
    /// Finished children kept at 64 messages by a transient blocker only.
    candidates: BTreeSet<String>,
    /// Sessions whose window an operation reads under the lock, counted.
    pins: HashMap<String, usize>,
}

impl ColdTranscriptState {
    /// Queues sessions for the per-pass recheck. Boot uses it for finished
    /// children that only a stored test-run card keeps resident: removing
    /// that card later mutates no session, so no pass would write them again.
    pub(crate) fn enroll_candidates(&mut self, session_ids: impl IntoIterator<Item = String>) {
        self.candidates.extend(session_ids);
    }

    pub(crate) fn is_candidate(&self, session_id: &str) -> bool {
        self.candidates.contains(session_id)
    }

    #[cfg(test)]
    pub(crate) fn has_pins(&self) -> bool {
        !self.pins.is_empty()
    }
}

/// Runtime form of the rule on a live record, using the published delegation
/// inventory. A hidden internal session, which visible-session reads cannot
/// serve, keeps its tail.
pub(crate) fn cold_trim_decision(
    inner: &StateInner,
    record: &SessionRecord,
    now: std::time::Instant,
) -> ColdTrimDecision {
    if !(record.is_local_session()
        && !record.hidden
        && record.session.parent_delegation_id.is_some()
        && finished_child_status(record.session.status)
        && matches!(record.runtime, SessionRuntime::None)
        && record.queued_prompts.is_empty()
        && record.queued_peer_messages.is_empty()
        && !session_needs_interruption_recovery(record)
        && record.restart_recovery.is_none())
    {
        return ColdTrimDecision::Never;
    }
    if !inner.delegation_inventory_complete() {
        return ColdTrimDecision::Retry;
    }
    let session_id = record.session.id.as_str();
    let mut child_links = 0;
    let mut settled = true;
    let mut card_pending = false;
    for delegation in &inner.delegations {
        if delegation.parent_session_id == session_id {
            if !delegation_is_terminal(delegation.status) {
                return ColdTrimDecision::Never;
            }
            // Its card is rewritten in this window when a pending submission
            // settles, which pins the window and wakes the recheck when the pin
            // drops (acceptance_evaluation_api.rs), or when the recorded result
            // is refreshed, which queues it (acceptance_evaluation_cards.rs).
            card_pending |= acceptance_card_may_still_change(
                delegation,
                inner
                    .acceptance_evaluation_submissions_in_flight
                    .contains_key(&delegation.id),
            );
            continue;
        }
        if delegation.child_session_id == session_id {
            child_links += 1;
            settled &= delegation_is_terminal(delegation.status);
        }
    }
    if child_links != 1 {
        return ColdTrimDecision::Never;
    }
    let held = !settled
        || card_pending
        || inner.cold_transcripts.pins.contains_key(session_id)
        // A card is updated, deduplicated and expired through its message,
        // terminal or not; the owner stays resident while the index has it.
        || inner
            .test_run_cards
            .values()
            .any(|card| card.session_id == session_id)
        // A late watcher capture may still amend this turn's file-change
        // card: the grace window is open, or a capture holds the tracking.
        || record
            .active_turn_file_change_grace_deadline
            .is_some_and(|deadline| now <= deadline)
        || record
            .file_change_tracking
            .as_ref()
            .is_some_and(|tracking| Arc::strong_count(tracking) > 1);
    if held {
        ColdTrimDecision::Retry
    } else {
        ColdTrimDecision::Trim
    }
}

impl StateInner {
    /// Queues an existing session for the per-pass recheck, for a change that
    /// may release it without writing the session itself.
    pub(crate) fn enroll_cold_transcript_recheck(&mut self, session_id: &str) {
        if self.find_session_index(session_id).is_some() {
            self.cold_transcripts
                .candidates
                .insert(session_id.to_owned());
        }
    }

    /// The resident window a committed persistence pass keeps for a session,
    /// recording a finished child that a transient blocker still holds so a
    /// later pass can trim it.
    pub(crate) fn cold_trim_limit(&mut self, index: usize) -> usize {
        let decision = cold_trim_decision(self, &self.sessions[index], std::time::Instant::now());
        let session_id = self.sessions[index].session.id.clone();
        match decision {
            ColdTrimDecision::Trim => {
                self.cold_transcripts.candidates.remove(&session_id);
                0
            }
            ColdTrimDecision::Retry => {
                self.cold_transcripts.candidates.insert(session_id);
                SESSION_IN_MEMORY_MESSAGE_LIMIT
            }
            ColdTrimDecision::Never => {
                self.cold_transcripts.candidates.remove(&session_id);
                SESSION_IN_MEMORY_MESSAGE_LIMIT
            }
        }
    }

    /// After a committed persistence pass, trims the candidates whose
    /// blockers have cleared and whose state is durable at `watermark`, and
    /// drops those that are gone or no longer finished children. The retired
    /// storage is returned so the caller drops it after releasing the lock.
    #[cfg(test)]
    pub(crate) fn trim_cold_transcript_candidates(
        &mut self,
        watermark: u64,
    ) -> Vec<(MessageSequence, HashMap<String, usize>)> {
        self.trim_cold_transcript_candidates_at(watermark, std::time::Instant::now())
    }

    /// The trim above, deciding every grace window at `now`.
    pub(crate) fn trim_cold_transcript_candidates_at(
        &mut self,
        watermark: u64,
        now: std::time::Instant,
    ) -> Vec<(MessageSequence, HashMap<String, usize>)> {
        let mut retired = Vec::new();
        let candidates = self
            .cold_transcripts
            .candidates
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for session_id in candidates {
            let Some(index) = self.find_session_index(&session_id) else {
                self.cold_transcripts.candidates.remove(&session_id);
                continue;
            };
            if self.sessions[index].mutation_stamp > watermark {
                // Not durable yet; its own persistence pass decides again.
                continue;
            }
            match cold_trim_decision(self, &self.sessions[index], now) {
                ColdTrimDecision::Retry => {}
                ColdTrimDecision::Never => {
                    self.cold_transcripts.candidates.remove(&session_id);
                }
                ColdTrimDecision::Trim => {
                    self.cold_transcripts.candidates.remove(&session_id);
                    retired.extend(detach_retained_session_messages(
                        &mut self.sessions[index],
                        0,
                    ));
                }
            }
        }
        retired
    }

    /// Just after the earliest grace window still open at `now`, the instant
    /// the trim decided at: no write may follow it, so the persistence worker
    /// wakes then to recheck the candidate.
    pub(crate) fn cold_transcript_grace_recheck_at(
        &self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.cold_transcripts
            .candidates
            .iter()
            .filter_map(|session_id| self.find_session_index(session_id))
            .filter_map(|index| self.sessions[index].active_turn_file_change_grace_deadline)
            .filter(|deadline| now <= *deadline)
            .min()
            .map(|deadline| deadline + std::time::Duration::from_millis(1))
    }
}

/// Keeps a session's resident window from being trimmed while an operation
/// that installed it reads it under the lock; released on drop, on every path.
pub(crate) struct ColdWindowPin<'a> {
    state: &'a AppState,
    session_id: String,
}

impl Drop for ColdWindowPin<'_> {
    fn drop(&mut self) {
        let Ok(mut inner) = self.state.inner.lock() else {
            return;
        };
        let released = match inner.cold_transcripts.pins.get_mut(&self.session_id) {
            Some(count) => {
                *count -= 1;
                *count == 0
            }
            None => false,
        };
        if released {
            inner.cold_transcripts.pins.remove(&self.session_id);
            // The window the pinned operation installed changed no stamp, so
            // no persistence pass would write this session again: wake a pass
            // to recheck it under the usual rule.
            if inner.find_session_index(&self.session_id).is_some() {
                inner
                    .cold_transcripts
                    .candidates
                    .insert(self.session_id.clone());
                let _ = self.state.persist_tx.send(PersistRequest::Delta);
            }
        }
    }
}

impl AppState {
    /// For a visible local session whose resident window is shorter than the
    /// one a settled turn is read from, reads that window from SQLite.
    /// Otherwise the resident window is current and nothing is read.
    pub(crate) fn read_cold_session_window(
        &self,
        session_id: &str,
    ) -> Result<Option<LocalSessionTailWindow>, ApiError> {
        let cold = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_visible_session_index(session_id)
                .map(|index| &inner.sessions[index])
                .is_some_and(|record| {
                    record.is_local_session()
                        && record.session.messages.len()
                            < SESSION_IN_MEMORY_MESSAGE_LIMIT
                                .min(session_message_count(record) as usize)
                })
        };
        if !cold {
            return Ok(None);
        }
        self.read_local_session_tail_window(session_id, SESSION_IN_MEMORY_MESSAGE_LIMIT)
            .map(Some)
    }

    /// Before loaded delegation records are published, reads the windows of
    /// the sessions that are cold although those records give them an open
    /// link (a child of a live delegation, or a parent that one may still
    /// read). Boot's prepass
    /// read the same rows, so this is normally empty; recovery and parent
    /// card updates must still never meet a cold window.
    pub(crate) fn read_windows_for_open_links(
        &self,
        records: &[DelegationRecord],
    ) -> Result<Vec<(String, LocalSessionTailWindow)>, ApiError> {
        let links = DelegationLinkCut::from_records(records);
        let cold_with_open_links = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter(|record| {
                    record.is_local_session()
                        && record.session.messages.len()
                            < SESSION_IN_MEMORY_MESSAGE_LIMIT
                                .min(session_message_count(record) as usize)
                        && !links.is_finished_child(&record.session.id)
                })
                .map(|record| record.session.id.clone())
                .collect::<Vec<_>>()
        };
        cold_with_open_links
            .into_iter()
            .map(|session_id| {
                self.read_local_session_tail_window(&session_id, SESSION_IN_MEMORY_MESSAGE_LIMIT)
                    .map(|window| (session_id, window))
            })
            .collect()
    }

    /// Makes a cold session's last window resident again, as it was before it
    /// went cold, for an operation that reads it under the state lock. The
    /// window is installed only for the record version it was read from. No
    /// persistence pass trims it while the returned pin lives; afterwards the
    /// usual rule applies again.
    pub(crate) fn hydrate_cold_session_window(
        &self,
        session_id: &str,
    ) -> Result<ColdWindowPin<'_>, ApiError> {
        // Pin first, so no persistence pass trims the window between the
        // read, the install and the operation's last recheck.
        let pin = self.pin_cold_session_window(session_id);
        if let Some(window) = self.read_cold_session_window(session_id)? {
            let installed = {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                match inner.find_session_index(session_id) {
                    Some(index) => install_cold_session_window(&mut inner.sessions[index], window),
                    None => Ok(()),
                }
            };
            installed?;
        }
        Ok(pin)
    }

    fn pin_cold_session_window(&self, session_id: &str) -> ColdWindowPin<'_> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        self.pin_cold_session_window_locked(&mut inner, session_id)
    }

    /// The pin above, taken under the state lock the caller holds, for an
    /// operation that releases that lock before it reads the window.
    pub(crate) fn pin_cold_session_window_locked(
        &self,
        inner: &mut StateInner,
        session_id: &str,
    ) -> ColdWindowPin<'_> {
        *inner
            .cold_transcripts
            .pins
            .entry(session_id.to_owned())
            .or_insert(0) += 1;
        ColdWindowPin {
            state: self,
            session_id: session_id.to_owned(),
        }
    }
}

/// Replaces a session's resident window with a window read from SQLite that
/// ends where the transcript ends. Resident bodies were authoritative in that
/// read, so nothing newer is lost; a different record version is refused.
pub(crate) fn install_cold_session_window(
    record: &mut SessionRecord,
    window: LocalSessionTailWindow,
) -> Result<(), ApiError> {
    let count = session_message_count(record) as usize;
    if record.mutation_stamp != window.session_mutation_stamp
        || window.start_index + window.messages.len() != count
        || window.start_index > record.message_start_index
    {
        return Err(ApiError::conflict(
            "the session changed while its transcript window was read; retry",
        ));
    }
    // Direct field access: the bodies are already durable, so installing
    // them is not a mutation to persist or publish.
    record.message_start_index = window.start_index;
    record.session.messages = window.messages.into();
    record.message_positions = build_message_positions(&record.session.messages);
    record.session.messages_loaded = window.start_index == 0;
    Ok(())
}

/// Puts a session into the cold shape: the whole transcript is durable, its
/// count is kept, and nothing of it is resident.
pub(crate) fn start_persisted_session_cold(record: &mut PersistedSessionRecord) {
    let total = record.session.message_count as usize;
    record.message_start_index = total;
    record.session.messages = MessageSequence::default();
    record.session.messages_loaded = total == 0;
}
