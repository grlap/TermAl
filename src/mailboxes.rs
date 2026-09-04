/*
Durable neutral mailbox storage.

Mailboxes are coordination records, not agent sessions: they have no runtime,
workdir, prompt queue, or model. SQLite is authoritative for message bodies and
participant cursors. This store owns one long-lived connection, independent of
the ordinary AppState persist worker, so mailbox append/read/ack remains usable
after that worker shuts down.
*/

const MAX_MAILBOX_BODY_BYTES: usize = 256 * 1024;
const MAX_MAILBOX_METADATA_BYTES: usize = 4 * 1024;
const MAILBOX_WRITER_ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct MailboxParticipant {
    session_id: String,
    display_name: String,
    processed_through: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    left_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct MailboxSummary {
    id: String,
    participants: Vec<MailboxParticipant>,
    latest_sequence: u64,
    unread_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_message_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_message_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct MailboxMessage {
    id: String,
    mailbox_id: String,
    sequence: u64,
    sender_session_id: String,
    sender_name: String,
    target_session_id: String,
    target_name: String,
    created_at: String,
    class: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_stamp: Option<String>,
    body: String,
    #[serde(default, skip_serializing)]
    idempotency_key: String,
    #[serde(default, skip_serializing)]
    unread_depth_at_append: u64,
    notification_state: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MailboxAppendInput {
    sender_session_id: String,
    sender_name: String,
    target_session_id: String,
    target_name: String,
    body: String,
    idempotency_key: String,
    topic: Option<String>,
    state_stamp: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MailboxIdempotencyRecord {
    mailbox_id: String,
    message_id: String,
    sequence: u64,
    sender_name: String,
    target_session_id: String,
    target_name: String,
    body: String,
    topic: Option<String>,
    state_stamp: Option<String>,
    unread_depth_at_append: u64,
    dispatch_outcome: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct MailboxAppendReceipt {
    mailbox_id: String,
    message_id: String,
    sequence: u64,
    unread_depth: u64,
    notification_disposition: String,
    duplicate: bool,
}

struct MailboxAppendResult {
    receipt: MailboxAppendReceipt,
    finalization: Option<MailboxDispatchFinalizationGuard>,
}

impl std::fmt::Debug for MailboxAppendResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MailboxAppendResult")
            .field("receipt", &self.receipt)
            .field("requires_finalization", &self.finalization.is_some())
            .finish()
    }
}

impl std::ops::Deref for MailboxAppendResult {
    type Target = MailboxAppendReceipt;

    fn deref(&self) -> &Self::Target {
        &self.receipt
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MailboxDispatchOutcomeRecord {
    Recorded { state_advanced: bool },
    AlreadyFinalized { dispatch_outcome: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MailboxUnreadWakeup {
    mailbox_id: String,
    message_id: String,
    sequence: u64,
    unread_count: u64,
    sender_session_id: String,
    sender_name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MailboxWakeQueueOutcome {
    accepted: bool,
    changed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MailboxWakeupRecovery {
    NeverWoken,
    AllUnreadAfterBoot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MailboxStoreErrorKind {
    Validation,
    Conflict,
    NotFound,
    Retryable,
}

#[derive(Debug)]
struct MailboxStoreError {
    kind: MailboxStoreErrorKind,
    message: String,
}

impl std::fmt::Display for MailboxStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MailboxStoreError {}

fn mailbox_store_error(
    kind: MailboxStoreErrorKind,
    message: impl Into<String>,
) -> anyhow::Error {
    MailboxStoreError {
        kind,
        message: message.into(),
    }
    .into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendMailboxMessageRequest {
    target_session_id: String,
    message: String,
    idempotency_key: String,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default)]
    state_stamp: Option<String>,
    #[serde(default)]
    class: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadMailboxRequest {
    #[serde(default)]
    after_sequence: u64,
    #[serde(default = "default_mailbox_read_limit")]
    limit: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcknowledgeMailboxRequest {
    expected_processed_through: u64,
    processed_through: u64,
}

fn default_mailbox_read_limit() -> u64 {
    50
}

#[derive(Default)]
struct MailboxDispatchFinalizationState {
    pending_message_ids: HashSet<String>,
    #[cfg(test)]
    waiters_by_message_id: HashMap<String, usize>,
}

#[derive(Default)]
struct MailboxDispatchFinalization {
    state: Mutex<MailboxDispatchFinalizationState>,
    changed: Condvar,
}

struct MailboxStore {
    connection: Mutex<Option<rusqlite::Connection>>,
    write_lock: Arc<SqliteStateWriterAdmission>,
    write_admission_timeout: Duration,
    dispatch_finalization: Arc<MailboxDispatchFinalization>,
    /// Serializes the durable structured-review append with recovery's
    /// read/probe decision. The main AppState mutex is never held across this
    /// coordination-store I/O; a conditional attempt check protects the later
    /// in-memory probe commit from a concurrent delegation rearm.
    delegation_review_recovery_guard: Mutex<()>,
    #[cfg(test)]
    internal_writer_waiters: Mutex<usize>,
    #[cfg(test)]
    internal_writer_waiter_changed: Condvar,
}

struct MailboxConnectionGuard<'a> {
    guard: std::sync::MutexGuard<'a, Option<rusqlite::Connection>>,
}

struct MailboxDispatchFinalizationGuard {
    dispatch_finalization: Arc<MailboxDispatchFinalization>,
    message_id: String,
}

#[cfg(test)]
fn decrement_dispatch_waiter(
    state: &mut MailboxDispatchFinalizationState,
    message_id: &str,
) {
    if let Some(waiters) = state.waiters_by_message_id.get_mut(message_id) {
        *waiters -= 1;
        if *waiters == 0 {
            state.waiters_by_message_id.remove(message_id);
        }
    }
}

impl Drop for MailboxDispatchFinalizationGuard {
    fn drop(&mut self) {
        // Panic-safe release: if dispatch unwinds after the durable append,
        // the NULL outcome remains the conservative durableButNotWoken
        // fallback and no same-process duplicate can stay parked forever.
        let mut state = self
            .dispatch_finalization
            .state
            .lock()
            .expect("mailbox dispatch finalization mutex poisoned");
        state.pending_message_ids.remove(&self.message_id);
        self.dispatch_finalization.changed.notify_all();
    }
}

impl std::ops::Deref for MailboxConnectionGuard<'_> {
    type Target = rusqlite::Connection;

    fn deref(&self) -> &Self::Target {
        self.guard
            .as_ref()
            .expect("enabled mailbox store should own a connection")
    }
}

impl std::ops::DerefMut for MailboxConnectionGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard
            .as_mut()
            .expect("enabled mailbox store should own a connection")
    }
}

impl AppState {
    /// Rejoins stale mailbox participant rows for a live local root session.
    ///
    /// `left_at` is deletion-owned state. Older send-time liveness probes
    /// could set it after a transient classification miss, so every ordinary
    /// mailbox interaction repairs those historical rows after validating
    /// the current in-memory session. The second validation closes the race
    /// with deletion or any other transition that makes the session ineligible;
    /// every failed revalidation restores `left_at`.
    fn ensure_mailbox_session_active(
        &self,
        session_id: &str,
    ) -> std::result::Result<(), ApiError> {
        let validate = || {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &inner.sessions[index];
            if record.hidden
                || !record.is_local_session()
                || record.session.parent_delegation_id.is_some()
                || inner
                    .find_delegation_index_by_child_session_id(session_id)
                    .is_some()
            {
                return Err(ApiError::bad_request(
                    "mailbox participant must be a local root session",
                ));
            }
            Ok(())
        };

        validate()?;
        let reactivated_rows = self
            .mailbox_store
            .reactivate_session_rows(session_id)
            .map_err(mailbox_api_error)?;
        if let Err(err) = validate() {
            if let Err(mark_err) = self
                .mailbox_store
                .restore_reactivated_session_rows(session_id, &reactivated_rows)
            {
                eprintln!(
                    "mailbox cleanup> failed restoring reactivated participant markers for \
                     `{session_id}` after concurrent recovery: {mark_err:#}"
                );
            }
            return Err(err);
        }
        Ok(())
    }

    fn append_mailbox_message_and_notify(
        &self,
        sender_session_id: &str,
        request: SendMailboxMessageRequest,
    ) -> std::result::Result<MailboxAppendReceipt, ApiError> {
        if request
            .class
            .as_deref()
            .is_some_and(|class| class != "routine")
        {
            return Err(ApiError::bad_request(
                "durable mailboxes currently support only class `routine`; STOP/urgent delivery is not active",
            ));
        }
        let (sender_name, target_name) =
            self.mailbox_peer_names(sender_session_id, &request.target_session_id)?;
        self.ensure_mailbox_session_active(sender_session_id)?;
        self.ensure_mailbox_session_active(&request.target_session_id)?;
        let input = MailboxAppendInput {
            sender_session_id: sender_session_id.to_owned(),
            sender_name: sender_name.clone(),
            target_session_id: request.target_session_id.clone(),
            target_name,
            body: request.message,
            idempotency_key: request.idempotency_key,
            topic: request.topic,
            state_stamp: request.state_stamp,
        };
        let appended = self
            .mailbox_store
            .append(&input)
            .map_err(mailbox_api_error)?;
        let MailboxAppendResult {
            mut receipt,
            finalization: _dispatch_finalization,
        } = appended;
        if receipt.duplicate {
            return Ok(receipt);
        }

        // This post-commit probe controls wake delivery only. It must never
        // mutate participant authorization: only deliberate session deletion
        // owns `left_at`.
        let (_, target_still_active) = self.mailbox_participants_still_active(
            sender_session_id,
            &input.target_session_id,
        );
        if !target_still_active {
            if let Err(err) = self
                .mailbox_store
                .record_initial_dispatch_outcome(
                    &receipt.message_id,
                    "durableButNotWoken",
                )
            {
                eprintln!(
                    "mailbox> message {} committed, but failed finalizing its durable receipt: {err:#}",
                    receipt.message_id
                );
            }
            return Ok(receipt);
        }

        let notification_text = mailbox_notification_text(
            &receipt.mailbox_id,
            receipt.unread_depth,
            receipt.sequence,
            &sender_name,
        );
        let notification_request = SendMessageRequest {
            text: notification_text,
            expanded_text: None,
            attachments: Vec::new(),
            source_session_id: Some(sender_session_id.to_owned()),
            source_mailbox: Some(MailboxMessageSource {
                mailbox_id: receipt.mailbox_id.clone(),
                message_id: receipt.message_id.clone(),
                sequence: receipt.sequence,
                unread_count: receipt.unread_depth,
            }),
        };

        let disposition = match self.dispatch_turn(&input.target_session_id, notification_request) {
            Ok(DispatchTurnResult::Dispatched(dispatch)) => {
                match deliver_turn_dispatch(self, dispatch) {
                    Ok(()) => Some("deliveredToIdleSession"),
                    Err(err) => {
                        eprintln!(
                            "mailbox> failed waking target session `{}` for mailbox `{}` message `{}` ({}): {}",
                            input.target_session_id,
                            receipt.mailbox_id,
                            receipt.message_id,
                            err.status,
                            err.message
                        );
                        None
                    }
                }
            }
            Ok(DispatchTurnResult::DispatchedAfterQueue(dispatch)) => {
                match deliver_turn_dispatch(self, dispatch) {
                    Ok(()) => Some("queuedBehindActiveTurn"),
                    Err(err) => {
                        eprintln!(
                            "mailbox> failed waking queued target session `{}` for mailbox `{}` message `{}` ({}): {}",
                            input.target_session_id,
                            receipt.mailbox_id,
                            receipt.message_id,
                            err.status,
                            err.message
                        );
                        None
                    }
                }
            }
            Ok(DispatchTurnResult::Queued) => Some("queuedBehindActiveTurn"),
            Err(err) => {
                eprintln!(
                    "mailbox> failed dispatching wake to target session `{}` for mailbox `{}` message `{}` ({}): {}",
                    input.target_session_id,
                    receipt.mailbox_id,
                    receipt.message_id,
                    err.status,
                    err.message
                );
                None
            }
        };
        let disposition = disposition.unwrap_or("durableButNotWoken");
        match self
            .mailbox_store
            .record_initial_dispatch_outcome(&receipt.message_id, disposition)
        {
            Ok(MailboxDispatchOutcomeRecord::Recorded { .. }) => {
                receipt.notification_disposition = disposition.to_owned();
            }
            Ok(MailboxDispatchOutcomeRecord::AlreadyFinalized {
                dispatch_outcome,
            }) => {
                receipt.notification_disposition = dispatch_outcome;
            }
            Err(err) => {
                eprintln!(
                    "mailbox> failed recording `{disposition}` for message `{}`: {err:#}",
                    receipt.message_id
                );
            }
        }
        Ok(receipt)
    }

    fn mailbox_peer_names(
        &self,
        sender_session_id: &str,
        target_session_id: &str,
    ) -> std::result::Result<(String, String), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let sender_index = inner
            .find_session_index(sender_session_id)
            .ok_or_else(|| ApiError::not_found("sender session not found"))?;
        let target_index = inner
            .find_session_index(target_session_id)
            .ok_or_else(|| ApiError::not_found("target session not found"))?;
        for (label, index) in [("sender", sender_index), ("target", target_index)] {
            let record = &inner.sessions[index];
            if record.hidden
                || !record.is_local_session()
                || record.session.parent_delegation_id.is_some()
                || inner
                    .find_delegation_index_by_child_session_id(&record.session.id)
                    .is_some()
            {
                return Err(ApiError::bad_request(format!(
                    "{label} must be a local root session"
                )));
            }
        }
        if sender_session_id == target_session_id {
            return Err(ApiError::bad_request(
                "mailbox messages must target another session",
            ));
        }
        Ok((
            inner.sessions[sender_index].session.name.clone(),
            inner.sessions[target_index].session.name.clone(),
        ))
    }

    fn mailbox_participants_still_active(
        &self,
        sender_session_id: &str,
        target_session_id: &str,
    ) -> (bool, bool) {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let active = |session_id: &str| {
            inner
                .find_session_index(session_id)
                .is_some_and(|index| {
                    let record = &inner.sessions[index];
                    !record.hidden
                        && record.is_local_session()
                        && record.session.parent_delegation_id.is_none()
                        && inner
                            .find_delegation_index_by_child_session_id(session_id)
                            .is_none()
                })
        };
        (active(sender_session_id), active(target_session_id))
    }

    fn reconcile_mailbox_wakeups_for_session(
        &self,
        session_id: &str,
        recovery: MailboxWakeupRecovery,
    ) -> Result<bool> {
        let wakeups = self
            .mailbox_store
            .wakeups_for_session(session_id, recovery)?;
        self.queue_mailbox_wakeups_for_session(session_id, wakeups, recovery)
    }

    fn requeue_rejected_mailbox_notification(
        &self,
        notification: &MailboxNotificationDelivery,
    ) -> Result<bool> {
        let Some(wakeup) = self.mailbox_store.unread_wakeup_for_mailbox_through(
            &notification.session_id,
            &notification.mailbox_id,
            notification.through_sequence,
        )? else {
            return Ok(false);
        };
        let outcome = self.queue_mailbox_wakeups_for_session_outcome(
            &notification.session_id,
            vec![wakeup],
            MailboxWakeupRecovery::NeverWoken,
            true,
        )?;
        if outcome.accepted {
            self.mailbox_store.mark_failed_delivery_recovered_through(
                &notification.session_id,
                &notification.mailbox_id,
                notification.through_sequence,
            )?;
        }
        Ok(outcome.changed)
    }

    fn queue_mailbox_wakeups_for_session(
        &self,
        session_id: &str,
        wakeups: Vec<MailboxUnreadWakeup>,
        recovery: MailboxWakeupRecovery,
    ) -> Result<bool> {
        Ok(self
            .queue_mailbox_wakeups_for_session_outcome(session_id, wakeups, recovery, false)?
            .changed)
    }

    fn queue_mailbox_wakeups_for_session_outcome(
        &self,
        session_id: &str,
        wakeups: Vec<MailboxUnreadWakeup>,
        recovery: MailboxWakeupRecovery,
        promote_existing_to_front: bool,
    ) -> Result<MailboxWakeQueueOutcome> {
        if wakeups.is_empty() {
            return Ok(MailboxWakeQueueOutcome::default());
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_visible_session_index(session_id) else {
            return Ok(MailboxWakeQueueOutcome::default());
        };
        if !inner.sessions[index].is_local_session()
            || inner.sessions[index].session.parent_delegation_id.is_some()
            || inner
                .find_delegation_index_by_child_session_id(session_id)
                .is_some()
        {
            return Ok(MailboxWakeQueueOutcome::default());
        }

        let mut changed = false;
        let mut recovered_through = Vec::with_capacity(wakeups.len());
        for wakeup in wakeups {
            recovered_through.push((wakeup.mailbox_id.clone(), wakeup.sequence));
            if inner.sessions[index]
                .active_turn_mailbox_notification
                .as_ref()
                .is_some_and(|active| {
                    active.mailbox_id == wakeup.mailbox_id
                        && active.through_sequence >= wakeup.sequence
                })
            {
                // A successor turn already owns this mailbox boundary. The
                // failed delivery is covered, but queuing it again would
                // duplicate the live wake after the terminal transition.
                continue;
            }
            let text = mailbox_notification_text(
                &wakeup.mailbox_id,
                wakeup.unread_count,
                wakeup.sequence,
                &wakeup.sender_name,
            );
            let source = MessageSource::mailbox(
                wakeup.sender_session_id,
                wakeup.sender_name,
                MailboxMessageSource {
                    mailbox_id: wakeup.mailbox_id.clone(),
                    message_id: wakeup.message_id,
                    sequence: wakeup.sequence,
                    unread_count: wakeup.unread_count,
                },
            );
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if let Some(existing_index) = record.queued_prompts.iter().position(|queued| {
                queued
                    .pending_prompt
                    .source
                    .as_ref()
                    .and_then(|candidate| candidate.mailbox.as_ref())
                    .is_some_and(|mailbox| mailbox.mailbox_id == wakeup.mailbox_id)
            }) {
                let mut existing = record
                    .queued_prompts
                    .remove(existing_index)
                    .expect("queued mailbox index should remain valid");
                let existing_sequence = existing
                    .pending_prompt
                    .source
                    .as_ref()
                    .and_then(|candidate| candidate.mailbox.as_ref())
                    .map_or(0, |mailbox| mailbox.sequence);
                if existing_sequence > wakeup.sequence {
                    // A narrower recovery query can legitimately find an older
                    // never-woken row while a newer wake is already queued.
                    // The existing wake covers that row; never regress the
                    // prompt's receipt metadata to the older sequence.
                } else if existing.pending_prompt.text != text
                    || existing.pending_prompt.source.as_ref() != Some(&source)
                {
                    existing.pending_prompt.timestamp = stamp_now();
                    existing.pending_prompt.text = text;
                    existing.pending_prompt.source = Some(source);
                    changed = true;
                }
                if promote_existing_to_front {
                    record.queued_prompts.push_front(existing);
                    changed |= existing_index != 0;
                } else {
                    record.queued_prompts.insert(existing_index, existing);
                }
                continue;
            }

            let prompt_id = inner.next_message_id();
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.queued_prompts.push_front(QueuedPromptRecord {
                source: QueuedPromptSource::Mailbox,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: prompt_id,
                    timestamp: stamp_now(),
                    text,
                    expanded_text: None,
                    source: Some(source),
                },
            });
            changed = true;
        }
        if changed {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            sync_pending_prompts(record);
            self.commit_locked(&mut inner)?;
        }
        drop(inner);
        for (mailbox_id, sequence) in recovered_through {
            self.mailbox_store.mark_notifications_recovered_through(
                session_id,
                &mailbox_id,
                sequence,
                recovery,
            )?;
        }
        Ok(MailboxWakeQueueOutcome {
            accepted: true,
            changed,
        })
    }

    fn reconcile_never_woken_mailbox_notifications_for_session(
        &self,
        session_id: &str,
    ) -> Result<bool> {
        self.reconcile_mailbox_wakeups_for_session(
            session_id,
            MailboxWakeupRecovery::NeverWoken,
        )
    }

    /// Revalidates the queue head against the durable participant cursor.
    ///
    /// A mailbox wake can sit behind a long-running turn while the receiver
    /// independently reads and acknowledges the same mailbox. Acknowledgement
    /// is authoritative, so a covered wake must never be promoted into a fresh
    /// agent turn. Returns `true` when the queue head changed and the caller
    /// should inspect it again.
    fn revalidate_front_mailbox_wakeup_for_session(&self, session_id: &str) -> Result<bool> {
        let Some((prompt_id, mailbox_id)) = ({
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions[index].queued_prompts.front())
                .and_then(|queued| {
                    queued
                        .pending_prompt
                        .source
                        .as_ref()
                        .and_then(|source| source.mailbox.as_ref())
                        .map(|mailbox| {
                            (
                                queued.pending_prompt.id.clone(),
                                mailbox.mailbox_id.clone(),
                            )
                        })
                })
        }) else {
            return Ok(false);
        };

        // Never hold the mailbox connection mutex together with StateInner.
        // The prompt id + sequence checks below make this optimistic read safe:
        // if another sender or acknowledgement wins the state lock first, we
        // retry against the new queue head.
        let wakeup = self
            .mailbox_store
            .unread_wakeup_for_mailbox(session_id, &mailbox_id)?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(false);
        };
        let front_matches = inner.sessions[index]
            .queued_prompts
            .front()
            .is_some_and(|queued| {
                queued.pending_prompt.id == prompt_id
                    && queued
                        .pending_prompt
                        .source
                        .as_ref()
                        .and_then(|source| source.mailbox.as_ref())
                        .is_some_and(|mailbox| mailbox.mailbox_id == mailbox_id)
            });
        if !front_matches {
            return Ok(true);
        }

        let Some(wakeup) = wakeup else {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            record.queued_prompts.pop_front();
            sync_pending_prompts(record);
            self.commit_locked(&mut inner)?;
            return Ok(true);
        };

        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let queued = record
            .queued_prompts
            .front_mut()
            .expect("validated mailbox queue head should exist");
        let existing_sequence = queued
            .pending_prompt
            .source
            .as_ref()
            .and_then(|source| source.mailbox.as_ref())
            .map_or(0, |mailbox| mailbox.sequence);
        if existing_sequence > wakeup.sequence {
            // A send committed after the optimistic store read and already
            // refreshed this prompt. Never regress it to the older snapshot.
            return Ok(false);
        }

        let text = mailbox_notification_text(
            &wakeup.mailbox_id,
            wakeup.unread_count,
            wakeup.sequence,
            &wakeup.sender_name,
        );
        let source = MessageSource::mailbox(
            wakeup.sender_session_id,
            wakeup.sender_name,
            MailboxMessageSource {
                mailbox_id: wakeup.mailbox_id,
                message_id: wakeup.message_id,
                sequence: wakeup.sequence,
                unread_count: wakeup.unread_count,
            },
        );
        if queued.pending_prompt.text != text
            || queued.pending_prompt.source.as_ref() != Some(&source)
            || queued.source != QueuedPromptSource::Mailbox
        {
            queued.pending_prompt.timestamp = stamp_now();
            queued.pending_prompt.text = text;
            queued.pending_prompt.source = Some(source);
            queued.source = QueuedPromptSource::Mailbox;
            sync_pending_prompts(record);
            self.commit_locked(&mut inner)?;
        }
        Ok(false)
    }

    /// Best-effort dispatch gate for stale mailbox wakes.
    ///
    /// Mailbox storage is a side channel and must not make ordinary prompt
    /// dispatch fail closed. A transient store error is logged and retried by
    /// the next dispatch/boot reconciliation.
    fn revalidate_queued_mailbox_wakeups_before_dispatch(&self, session_id: &str) {
        loop {
            match self.revalidate_front_mailbox_wakeup_for_session(session_id) {
                Ok(true) => continue,
                Ok(false) => return,
                Err(err) => {
                    eprintln!(
                        "mailbox> failed revalidating queued notification for `{session_id}`: {err:#}"
                    );
                    return;
                }
            }
        }
    }

    /// Removes queued wakes already covered by a successful acknowledgement.
    ///
    /// Dispatch-time revalidation remains authoritative across crashes. This
    /// eager sweep avoids retaining visibly stale queue entries during normal
    /// operation and establishes a clear lock winner for concurrent ack/send.
    fn remove_acknowledged_mailbox_wakeups(
        &self,
        session_id: &str,
        mailbox_id: &str,
        processed_through: u64,
    ) -> Result<bool> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(false);
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let original_len = record.queued_prompts.len();
        record.queued_prompts.retain(|queued| {
            !queued
                .pending_prompt
                .source
                .as_ref()
                .and_then(|source| source.mailbox.as_ref())
                .is_some_and(|mailbox| {
                    mailbox.mailbox_id == mailbox_id
                        && mailbox.sequence <= processed_through
                })
        });
        if record.queued_prompts.len() == original_len {
            return Ok(false);
        }
        sync_pending_prompts(record);
        self.commit_locked(&mut inner)?;
        Ok(true)
    }

    fn acknowledge_mailbox_and_remove_covered_wakeups(
        &self,
        session_id: &str,
        mailbox_id: &str,
        expected_processed_through: u64,
        processed_through: u64,
    ) -> Result<MailboxSummary> {
        let summary = self.mailbox_store.acknowledge(
            session_id,
            mailbox_id,
            expected_processed_through,
            processed_through,
        )?;
        if let Err(err) =
            self.remove_acknowledged_mailbox_wakeups(session_id, mailbox_id, processed_through)
        {
            // The durable CAS already committed. Returning an error would make
            // a correct retry conflict on the old expected cursor; dispatch-
            // time revalidation is the authoritative fallback.
            eprintln!(
                "mailbox> acknowledgement committed but queued-wake cleanup failed for \
                 `{session_id}` / `{mailbox_id}`: {err:#}"
            );
        }
        Ok(summary)
    }

    fn reconcile_unread_mailbox_wakeups_after_boot(&self) {
        let session_ids = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter(|record| {
                    !record.hidden
                        && record.is_local_session()
                        && record.session.parent_delegation_id.is_none()
                        && inner
                            .find_delegation_index_by_child_session_id(&record.session.id)
                            .is_none()
                })
                .map(|record| record.session.id.clone())
                .collect::<Vec<_>>()
        };
        for session_id in session_ids {
            if let Err(err) = self.reconcile_mailbox_wakeups_for_session(
                &session_id,
                MailboxWakeupRecovery::AllUnreadAfterBoot,
            ) {
                eprintln!(
                    "mailbox> failed recovering unread notification for `{session_id}` after boot: {err:#}"
                );
            }
        }
    }

    fn mark_mailbox_notification_delivered(
        &self,
        notification: &MailboxNotificationDelivery,
    ) {
        if let Err(err) = self.mailbox_store.mark_notifications_delivered_through(
            &notification.session_id,
            &notification.mailbox_id,
            notification.through_sequence,
        ) {
            eprintln!(
                "mailbox> failed marking notification delivered through #{} for `{}`: {err:#}",
                notification.through_sequence, notification.session_id
            );
        }
    }
}

fn mailbox_notification_text(
    mailbox_id: &str,
    unread_count: u64,
    sequence: u64,
    sender_name: &str,
) -> String {
    format!(
        "[TermAl mailbox notification]\n\
         Mailbox `{mailbox_id}` has {unread_count} unread message(s). Latest inbound: #{sequence} from {sender_name}.\n\
         First use `termal_list_mailboxes` to obtain your current `processedThrough` cursor, \
         then use `termal_read_mailbox` with this mailbox id to fetch durable message bodies. \
         After processing, call `termal_acknowledge_mailbox` with that cursor as \
         `expectedProcessedThrough`. \
         If the TermAl MCP tools are unavailable, invoke the executable in `TERMAL_CLI` \
         from the shell. `TERMAL_SESSION_ID` and `TERMAL_BASE_URL` supply the CLI defaults; \
         follow `mailbox list` -> `mailbox read --after <processedThrough>` -> process/reply \
         with a stable idempotency key -> `mailbox acknowledge --expected <processedThrough>`."
    )
}

async fn send_mailbox_message(
    AxumPath(sender_session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<SendMailboxMessageRequest>,
) -> Result<(StatusCode, Json<MailboxAppendReceipt>), ApiError> {
    let receipt = run_blocking_api(move || {
        state.append_mailbox_message_and_notify(&sender_session_id, request)
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}

async fn submit_delegation_review_result(
    AxumPath(child_session_id): AxumPath<String>,
    State(state): State<AppState>,
    request: Result<Json<SubmitDelegationReviewResultRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<MailboxAppendReceipt>), ApiError> {
    let Json(request) = request
        .map_err(|rejection| api_json_rejection("delegation review result", rejection))?;
    let receipt = run_blocking_api(move || {
        state.submit_delegation_review_result(&child_session_id, request)
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}

async fn list_mailboxes(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<MailboxSummary>>, ApiError> {
    let summaries = run_blocking_api(move || {
        state.ensure_mailbox_session_active(&session_id)?;
        state
            .mailbox_store
            .list_for_session(&session_id)
            .map_err(mailbox_api_error)
    })
    .await?;
    Ok(Json(summaries))
}

async fn read_mailbox(
    AxumPath((session_id, mailbox_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<ReadMailboxRequest>,
) -> Result<Json<Vec<MailboxMessage>>, ApiError> {
    let messages = run_blocking_api(move || {
        state.ensure_mailbox_session_active(&session_id)?;
        state
            .mailbox_store
            .read_range(
                &session_id,
                &mailbox_id,
                request.after_sequence,
                request.limit,
            )
            .map_err(mailbox_api_error)
    })
    .await?;
    Ok(Json(messages))
}

async fn read_mailbox_message(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<MailboxMessage>, ApiError> {
    let message = run_blocking_api(move || {
        state.ensure_mailbox_session_active(&session_id)?;
        state
            .mailbox_store
            .read_message(&session_id, &message_id)
            .map_err(mailbox_api_error)
    })
    .await?;
    Ok(Json(message))
}

async fn acknowledge_mailbox(
    AxumPath((session_id, mailbox_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<AcknowledgeMailboxRequest>,
) -> Result<Json<MailboxSummary>, ApiError> {
    let summary = run_blocking_api(move || {
        state.ensure_mailbox_session_active(&session_id)?;
        state
            .acknowledge_mailbox_and_remove_covered_wakeups(
                &session_id,
                &mailbox_id,
                request.expected_processed_through,
                request.processed_through,
            )
            .map_err(mailbox_api_error)
    })
    .await?;
    Ok(Json(summary))
}

fn mailbox_api_error(err: anyhow::Error) -> ApiError {
    if let Some(mailbox_error) = err.downcast_ref::<MailboxStoreError>() {
        return match mailbox_error.kind {
            MailboxStoreErrorKind::Validation => {
                ApiError::bad_request(mailbox_error.message.clone())
            }
            MailboxStoreErrorKind::Conflict => {
                ApiError::conflict(mailbox_error.message.clone())
            }
            MailboxStoreErrorKind::NotFound => {
                ApiError::not_found(mailbox_error.message.clone())
            }
            MailboxStoreErrorKind::Retryable => ApiError::from_status(
                StatusCode::SERVICE_UNAVAILABLE,
                mailbox_error.message.clone(),
            ),
        };
    }
    ApiError::internal(format!("mailbox operation failed: {err:#}"))
}

impl MailboxStore {
    #[cfg(test)]
    fn open(path: &FsPath) -> Result<Self> {
        Self::open_with_write_admission_timeout(path, MAILBOX_WRITER_ADMISSION_TIMEOUT)
    }

    #[cfg(test)]
    fn open_with_write_admission_timeout(
        path: &FsPath,
        write_admission_timeout: Duration,
    ) -> Result<Self> {
        let connection = open_sqlite_state_connection(path)?;
        ensure_sqlite_coordination_schema_for_path(&connection, path)?;
        Self::from_validated_connection(path, connection, write_admission_timeout)
    }

    fn from_validated_connection(
        path: &FsPath,
        connection: rusqlite::Connection,
        write_admission_timeout: Duration,
    ) -> Result<Self> {
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .with_context(|| {
                format!(
                    "failed to enable mailbox foreign keys for `{}`",
                    path.display()
                )
        })?;
        Ok(Self {
            connection: Mutex::new(Some(connection)),
            write_lock: sqlite_state_write_lock(path),
            write_admission_timeout,
            dispatch_finalization: Arc::new(MailboxDispatchFinalization::default()),
            delegation_review_recovery_guard: Mutex::new(()),
            #[cfg(test)]
            internal_writer_waiters: Mutex::new(0),
            #[cfg(test)]
            internal_writer_waiter_changed: Condvar::new(),
        })
    }

    #[cfg(test)]
    fn disabled_for_tests() -> Self {
        Self {
            connection: Mutex::new(None),
            write_lock: Arc::new(SqliteStateWriterAdmission::default()),
            write_admission_timeout: MAILBOX_WRITER_ADMISSION_TIMEOUT,
            dispatch_finalization: Arc::new(MailboxDispatchFinalization::default()),
            delegation_review_recovery_guard: Mutex::new(()),
            internal_writer_waiters: Mutex::new(0),
            internal_writer_waiter_changed: Condvar::new(),
        }
    }

    fn lock_writer(&self, operation: &str) -> Result<SqliteStateWriterGuard<'_>> {
        lock_sqlite_state_writer_for(&self.write_lock, self.write_admission_timeout).ok_or_else(
            || {
                mailbox_store_error(
                    MailboxStoreErrorKind::Retryable,
                    format!(
                        "mailbox storage is temporarily busy while {operation}; no mailbox write \
                         was committed by this operation, so retry the same request"
                    ),
                )
            },
        )
    }

    fn append_delegation_review_result(
        &self,
        input: &MailboxAppendInput,
    ) -> Result<MailboxAppendResult> {
        let _recovery_guard = self
            .delegation_review_recovery_guard
            .lock()
            .expect("delegation review recovery guard mutex poisoned");
        self.append(input)
    }

    fn read_delegation_review_result_for_recovery(
        &self,
        sender_session_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<MailboxIdempotencyRecord>> {
        let _recovery_guard = self
            .delegation_review_recovery_guard
            .lock()
            .expect("delegation review recovery guard mutex poisoned");
        self.read_idempotent_message(sender_session_id, idempotency_key)
    }

    fn lock_internal_writer(&self) -> SqliteStateWriterGuard<'_> {
        // Request-owned append/ack paths have a bounded admission deadline so
        // they can return a retryable 503. These lifecycle writes run after a
        // durable commit or runtime acceptance and have no caller that can
        // safely retry them, so wait for the short in-process transaction
        // boundary instead of silently abandoning the state transition.
        #[cfg(test)]
        {
            let mut waiters = self
                .internal_writer_waiters
                .lock()
                .expect("mailbox internal writer waiter mutex poisoned");
            *waiters += 1;
            self.internal_writer_waiter_changed.notify_all();
        }
        let guard = lock_sqlite_state_writer(&self.write_lock);
        #[cfg(test)]
        {
            let mut waiters = self
                .internal_writer_waiters
                .lock()
                .expect("mailbox internal writer waiter mutex poisoned");
            *waiters -= 1;
            self.internal_writer_waiter_changed.notify_all();
        }
        guard
    }

    fn register_pending_dispatch_outcome(&self, message_id: &str) {
        self.dispatch_finalization
            .state
            .lock()
            .expect("mailbox dispatch finalization mutex poisoned")
            .pending_message_ids
            .insert(message_id.to_owned());
    }

    fn finish_pending_dispatch_outcome(&self, message_id: &str) {
        let mut state = self
            .dispatch_finalization
            .state
            .lock()
            .expect("mailbox dispatch finalization mutex poisoned");
        state.pending_message_ids.remove(message_id);
        self.dispatch_finalization.changed.notify_all();
    }

    fn wait_for_final_dispatch_outcome(&self, message_id: &str) -> Result<String> {
        let mut state = self
            .dispatch_finalization
            .state
            .lock()
            .expect("mailbox dispatch finalization mutex poisoned");
        if state.pending_message_ids.contains(message_id) {
            #[cfg(test)]
            {
            *state
                .waiters_by_message_id
                .entry(message_id.to_owned())
                .or_default() += 1;
            self.dispatch_finalization.changed.notify_all();
            }
            let deadline = std::time::Instant::now() + self.write_admission_timeout;
            while state.pending_message_ids.contains(message_id) {
                let Some(remaining) =
                    deadline.checked_duration_since(std::time::Instant::now())
                else {
                    #[cfg(test)]
                    decrement_dispatch_waiter(&mut state, message_id);
                    return Err(mailbox_store_error(
                        MailboxStoreErrorKind::Retryable,
                        "mailbox dispatch outcome is still finalizing; the original mailbox append \
                         is durable and replaying the same idempotency key is safe",
                    ));
                };
                let (next_state, timeout) = self
                    .dispatch_finalization
                    .changed
                    .wait_timeout(state, remaining)
                    .expect("mailbox dispatch finalization mutex poisoned");
                state = next_state;
                if timeout.timed_out() && state.pending_message_ids.contains(message_id) {
                    #[cfg(test)]
                    decrement_dispatch_waiter(&mut state, message_id);
                    return Err(mailbox_store_error(
                        MailboxStoreErrorKind::Retryable,
                        "mailbox dispatch outcome is still finalizing; the original mailbox append \
                         is durable and replaying the same idempotency key is safe",
                    ));
                }
            }
            #[cfg(test)]
            decrement_dispatch_waiter(&mut state, message_id);
        }
        drop(state);

        let connection = self.connection()?;
        let dispatch_outcome = connection
            .query_row(
                "SELECT dispatch_outcome
                 FROM mailbox_messages
                 WHERE id = ?1",
                rusqlite::params![message_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => {
                    anyhow!("mailbox message `{message_id}` disappeared during dispatch")
                }
                other => anyhow!(other).context("failed to read finalized mailbox dispatch outcome"),
            })?;
        Ok(dispatch_outcome.unwrap_or_else(|| "durableButNotWoken".to_owned()))
    }

    #[cfg(test)]
    fn wait_for_dispatch_outcome_waiter(&self, message_id: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut state = self
            .dispatch_finalization
            .state
            .lock()
            .expect("mailbox dispatch finalization mutex poisoned");
        while state
            .waiters_by_message_id
            .get(message_id)
            .copied()
            .unwrap_or(0)
            == 0
        {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("duplicate append did not reach the finalization wait boundary");
            let (next_state, timeout) = self
                .dispatch_finalization
                .changed
                .wait_timeout(state, remaining)
                .expect("mailbox dispatch finalization mutex poisoned");
            state = next_state;
            assert!(
                !timeout.timed_out(),
                "duplicate append did not reach the finalization wait boundary"
            );
        }
    }

    #[cfg(test)]
    fn wait_for_internal_writer_waiter(&self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut waiters = self
            .internal_writer_waiters
            .lock()
            .expect("mailbox internal writer waiter mutex poisoned");
        while *waiters == 0 {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("lifecycle writer did not reach the shared writer boundary");
            let (next_waiters, timeout) = self
                .internal_writer_waiter_changed
                .wait_timeout(waiters, remaining)
                .expect("mailbox internal writer waiter mutex poisoned");
            waiters = next_waiters;
            assert!(
                !timeout.timed_out(),
                "lifecycle writer did not reach the shared writer boundary"
            );
        }
    }

    fn connection_if_enabled(&self) -> Option<MailboxConnectionGuard<'_>> {
        let guard = self
            .connection
            .lock()
            .expect("mailbox connection mutex poisoned");
        guard
            .is_some()
            .then_some(MailboxConnectionGuard { guard })
    }

    fn connection(&self) -> Result<MailboxConnectionGuard<'_>> {
        self.connection_if_enabled()
            .ok_or_else(|| anyhow!("mailbox storage is disabled in this test state"))
    }

    fn read_idempotent_message(
        &self,
        sender_session_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<MailboxIdempotencyRecord>> {
        let connection = self.connection()?;
        mailbox_message_for_idempotency_key(&connection, sender_session_id, idempotency_key)
    }

    fn append(&self, input: &MailboxAppendInput) -> Result<MailboxAppendResult> {
        validate_mailbox_append_input(input)?;
        let write_guard = self.lock_writer("waiting to begin mailbox append")?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "beginning mailbox append")?;

        if let Some(existing) =
            mailbox_message_for_idempotency_key(&transaction, &input.sender_session_id, &input.idempotency_key)?
        {
            if existing.target_session_id != input.target_session_id
                || existing.body != input.body
                || existing.topic != input.topic
                || existing.state_stamp != input.state_stamp
            {
                return Err(mailbox_store_error(
                    MailboxStoreErrorKind::Conflict,
                    format!(
                        "idempotency key `{}` was already used for a different mailbox message",
                        input.idempotency_key
                    ),
                ));
            }
            transaction
                .commit()
                .map_err(|err| {
                    mailbox_sqlite_write_error("finishing duplicate mailbox lookup", err)
                })?;
            // Lock order: a duplicate may wait on dispatch finalization only after
            // releasing both SQLite resources and the shared writer-admission guard.
            drop(connection);
            drop(write_guard);
            let notification_disposition = match existing.dispatch_outcome {
                Some(dispatch_outcome) => dispatch_outcome,
                None => self.wait_for_final_dispatch_outcome(&existing.message_id)?,
            };
            return Ok(MailboxAppendResult {
                receipt: MailboxAppendReceipt {
                    mailbox_id: existing.mailbox_id,
                    message_id: existing.message_id,
                    sequence: existing.sequence,
                    unread_depth: existing.unread_depth_at_append,
                    notification_disposition,
                    duplicate: true,
                },
                finalization: None,
            });
        }

        let now = chrono::Utc::now().to_rfc3339();
        let participant_key =
            mailbox_participant_key(&input.sender_session_id, &input.target_session_id);
        let mailbox_id = match transaction.query_row(
            "SELECT id FROM mailboxes WHERE participant_key = ?1",
            rusqlite::params![&participant_key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(id) => id,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                let id = format!("mailbox-{}", Uuid::new_v4());
                transaction
                    .execute(
                        "INSERT INTO mailboxes(id, participant_key, created_at, next_sequence)
                         VALUES(?1, ?2, ?3, 1)",
                        rusqlite::params![&id, &participant_key, &now],
                    )
                    .context("failed to create mailbox")?;
                id
            }
            Err(err) => return Err(err).context("failed to find mailbox"),
        };

        upsert_mailbox_participant(
            &transaction,
            &mailbox_id,
            &input.sender_session_id,
            &input.sender_name,
            &now,
        )?;
        upsert_mailbox_participant(
            &transaction,
            &mailbox_id,
            &input.target_session_id,
            &input.target_name,
            &now,
        )?;

        let sequence = transaction
            .query_row(
                "UPDATE mailboxes
                 SET next_sequence = next_sequence + 1
                 WHERE id = ?1
                 RETURNING next_sequence - 1",
                rusqlite::params![&mailbox_id],
                |row| row.get::<_, u64>(0),
            )
            .context("failed to allocate mailbox sequence")?;
        let message_id = format!("mailbox-message-{}", Uuid::new_v4());
        let notification_disposition = "durableButNotWoken";
        let processed_through = transaction
            .query_row(
                "SELECT processed_through
                 FROM mailbox_participants
                 WHERE mailbox_id = ?1 AND session_id = ?2 AND left_at IS NULL",
                rusqlite::params![&mailbox_id, &input.target_session_id],
                |row| row.get::<_, u64>(0),
            )
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => mailbox_store_error(
                    MailboxStoreErrorKind::Conflict,
                    "target session is a departed mailbox participant",
                ),
                other => anyhow!(other).context("failed to read target mailbox cursor"),
            })?;
        let unread_depth = transaction
            .query_row(
                "SELECT COUNT(*) + 1
                 FROM mailbox_messages
                 WHERE mailbox_id = ?1
                   AND target_session_id = ?2
                   AND sequence > ?3",
                rusqlite::params![
                    &mailbox_id,
                    &input.target_session_id,
                    processed_through
                ],
                |row| row.get::<_, u64>(0),
            )
            .context("failed to count inbound unread mailbox messages")?;
        transaction
            .execute(
                "INSERT INTO mailbox_messages(
                   id, mailbox_id, sequence, sender_session_id, sender_name,
                   target_session_id, target_name, created_at, class, topic,
                   state_stamp, body, idempotency_key, unread_depth_at_append,
                   notification_disposition, dispatch_outcome
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'routine', ?9, ?10, ?11, ?12, ?13, ?14, NULL)",
                rusqlite::params![
                    &message_id,
                    &mailbox_id,
                    sequence,
                    &input.sender_session_id,
                    &input.sender_name,
                    &input.target_session_id,
                    &input.target_name,
                    &now,
                    &input.topic,
                    &input.state_stamp,
                    &input.body,
                    &input.idempotency_key,
                    unread_depth,
                    notification_disposition,
                ],
            )
            .context("failed to append mailbox message")?;
        // A participant cannot be "unread" on a message it wrote itself, so
        // the sender's cursor follows its own append — otherwise
        // `unreadCount` (which already excludes own sends) and
        // `processedThrough` (which previously only moved on an explicit ack)
        // disagree, and the natural "ack the sequence I last participated in"
        // call fails CAS with a spurious 409.
        //
        // The `processed_through = sequence - 1` guard is the entire safety
        // property: sequences are dense per mailbox, so the sender's cursor
        // may only follow its own message when it was ALREADY caught up to
        // the message immediately before it. If any peer message is still
        // unread below this one, the guard fails, the cursor stays put, and
        // that peer message cannot be silently consumed by an unrelated send.
        transaction
            .execute(
                "UPDATE mailbox_participants
                 SET processed_through = ?3
                 WHERE mailbox_id = ?1
                   AND session_id = ?2
                   AND left_at IS NULL
                   AND processed_through = ?3 - 1",
                rusqlite::params![&mailbox_id, &input.sender_session_id, sequence],
            )
            .context("failed to advance sender mailbox cursor")?;
        self.register_pending_dispatch_outcome(&message_id);
        let finalization = MailboxDispatchFinalizationGuard {
            dispatch_finalization: self.dispatch_finalization.clone(),
            message_id: message_id.clone(),
        };
        if let Err(err) = transaction.commit() {
            return Err(mailbox_sqlite_write_error(
                "committing mailbox append",
                err,
            ));
        }
        drop(write_guard);

        Ok(MailboxAppendResult {
            receipt: MailboxAppendReceipt {
                mailbox_id,
                message_id,
                sequence,
                unread_depth,
                notification_disposition: notification_disposition.to_owned(),
                duplicate: false,
            },
            finalization: Some(finalization),
        })
    }

    fn record_initial_dispatch_outcome(
        &self,
        message_id: &str,
        dispatch_outcome: &str,
    ) -> Result<MailboxDispatchOutcomeRecord> {
        let result = (|| {
            let _write_guard = self.lock_internal_writer();
            let mut connection = self.connection()?;
            let transaction = begin_mailbox_write(
                &mut connection,
                "beginning initial mailbox dispatch outcome update",
            )?;
            let outcome_updated = transaction
                .execute(
                    "UPDATE mailbox_messages
                     SET dispatch_outcome = ?2
                     WHERE id = ?1
                       AND dispatch_outcome IS NULL",
                    rusqlite::params![message_id, dispatch_outcome],
                )
                .context("failed to record initial mailbox dispatch outcome")?;
            if outcome_updated == 0 {
                let existing_outcome = transaction
                    .query_row(
                        "SELECT dispatch_outcome
                         FROM mailbox_messages
                         WHERE id = ?1",
                        rusqlite::params![message_id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .map_err(|err| match err {
                        rusqlite::Error::QueryReturnedNoRows => {
                            anyhow!("mailbox message `{message_id}` does not exist")
                        }
                        other => anyhow!(other).context("failed to read finalized mailbox outcome"),
                    })?
                    .context("finalized mailbox outcome should not be NULL")?;
                transaction
                    .commit()
                    .map_err(|err| {
                        mailbox_sqlite_write_error(
                            "committing duplicate mailbox dispatch finalization",
                            err,
                        )
                    })?;
                return Ok(MailboxDispatchOutcomeRecord::AlreadyFinalized {
                    dispatch_outcome: existing_outcome,
                });
            }
            let state_advanced = transaction
                .execute(
                    "UPDATE mailbox_messages
                     SET notification_disposition = ?2
                     WHERE id = ?1
                       AND notification_disposition = 'durableButNotWoken'",
                    rusqlite::params![message_id, dispatch_outcome],
                )
                .context("failed to advance initial mailbox notification state")?;
            transaction
                .commit()
                .map_err(|err| {
                    mailbox_sqlite_write_error(
                        "committing initial mailbox dispatch outcome update",
                        err,
                    )
                })?;
            Ok(MailboxDispatchOutcomeRecord::Recorded {
                state_advanced: state_advanced > 0,
            })
        })();
        // Finalizers persist while holding the writer boundary, release it when the
        // closure returns, and only then wake duplicate-receipt waiters.
        self.finish_pending_dispatch_outcome(message_id);
        result
    }

    #[cfg(test)]
    fn set_notification_state(&self, message_id: &str, notification_state: &str) -> Result<()> {
        let _write_guard = self.lock_writer("updating mailbox notification state")?;
        let connection = self.connection()?;
        let updated = connection
            .execute(
                "UPDATE mailbox_messages
                 SET notification_disposition = ?2
                 WHERE id = ?1",
                rusqlite::params![message_id, notification_state],
            )
            .context("failed to update mailbox notification state")?;
        if updated == 0 {
            bail!("mailbox message `{message_id}` does not exist");
        }
        Ok(())
    }

    fn mark_notifications_delivered_through(
        &self,
        session_id: &str,
        mailbox_id: &str,
        through_sequence: u64,
    ) -> Result<usize> {
        let _write_guard = self.lock_internal_writer();
        let connection = self.connection()?;
        let updated = connection
            .execute(
                "UPDATE mailbox_messages
                 SET notification_disposition = 'deliveredToIdleSession'
                 WHERE mailbox_id = ?1
                   AND target_session_id = ?2
                   AND sequence <= ?3
                   AND notification_disposition != 'deliveredToIdleSession'",
                rusqlite::params![mailbox_id, session_id, through_sequence],
            )
            .context("failed to mark mailbox notifications delivered")?;
        Ok(updated)
    }

    fn mark_notifications_recovered_through(
        &self,
        session_id: &str,
        mailbox_id: &str,
        through_sequence: u64,
        recovery: MailboxWakeupRecovery,
    ) -> Result<usize> {
        // Reactivation is initiated by HTTP/MCP mailbox requests. Use the
        // bounded request admission path so contention returns the existing
        // retryable 503 instead of pinning a request thread indefinitely.
        let _write_guard = self.lock_writer("reactivating a mailbox participant")?;
        let connection = self.connection()?;
        let updated = match recovery {
            MailboxWakeupRecovery::NeverWoken => connection.execute(
                "UPDATE mailbox_messages
                 SET notification_disposition = 'recoveredWake'
                 WHERE mailbox_id = ?1
                   AND target_session_id = ?2
                   AND sequence <= ?3
                   AND notification_disposition = 'durableButNotWoken'",
                rusqlite::params![mailbox_id, session_id, through_sequence],
            ),
            MailboxWakeupRecovery::AllUnreadAfterBoot => connection.execute(
                "UPDATE mailbox_messages
                 SET notification_disposition = 'recoveredWake'
                 WHERE mailbox_id = ?1
                   AND target_session_id = ?2
                   AND EXISTS (
                     SELECT 1
                     FROM mailbox_participants participant
                     WHERE participant.mailbox_id = mailbox_messages.mailbox_id
                       AND participant.session_id = mailbox_messages.target_session_id
                       AND participant.left_at IS NULL
                       AND mailbox_messages.sequence > participant.processed_through
                   )
                   AND sequence <= ?3
                   AND notification_disposition != 'recoveredWake'",
                rusqlite::params![mailbox_id, session_id, through_sequence],
            ),
        }
        .with_context(|| {
            format!("failed to mark mailbox notifications recovered during {recovery:?}")
        })?;
        Ok(updated)
    }

    /// Marks only the delivered boundary owned by the turn that actually
    /// failed. This is deliberately not a session-wide recovery scan: a newer
    /// accepted mailbox turn may already exist on the same session.
    fn mark_failed_delivery_recovered_through(
        &self,
        session_id: &str,
        mailbox_id: &str,
        through_sequence: u64,
    ) -> Result<usize> {
        let _write_guard = self.lock_writer("recovering a failed mailbox delivery")?;
        let connection = self.connection()?;
        connection
            .execute(
                "UPDATE mailbox_messages
                 SET notification_disposition = 'recoveredWake'
                 WHERE mailbox_id = ?1
                   AND target_session_id = ?2
                   AND sequence <= ?3
                   AND sequence > COALESCE((
                     SELECT processed_through
                     FROM mailbox_participants
                     WHERE mailbox_id = ?1
                       AND session_id = ?2
                       AND left_at IS NULL
                   ), 0)
                   AND notification_disposition = 'deliveredToIdleSession'",
                rusqlite::params![mailbox_id, session_id, through_sequence],
            )
            .context("failed to mark the failed mailbox delivery recovered")
    }

    fn mark_session_left(&self, session_id: &str) -> Result<()> {
        let _write_guard = self.lock_internal_writer();
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(());
        };
        connection
            .execute(
                "UPDATE mailbox_participants
                 SET left_at = COALESCE(left_at, ?2)
                 WHERE session_id = ?1",
                rusqlite::params![session_id, chrono::Utc::now().to_rfc3339()],
            )
            .context("failed to mark deleted mailbox participant as left")?;
        Ok(())
    }

    #[cfg(test)]
    fn reactivate_session(&self, session_id: &str) -> Result<usize> {
        self.reactivate_session_rows(session_id)
            .map(|rows| rows.len())
    }

    /// Clears stale departure markers and returns exactly the rows changed so
    /// a concurrent eligibility failure can restore only those markers.
    fn reactivate_session_rows(&self, session_id: &str) -> Result<Vec<(String, String)>> {
        let has_departed_rows = {
            let Some(connection) = self.connection_if_enabled() else {
                return Ok(Vec::new());
            };
            connection
                .query_row(
                    "SELECT EXISTS(
                       SELECT 1
                       FROM mailbox_participants
                       WHERE session_id = ?1
                         AND left_at IS NOT NULL
                     )",
                    rusqlite::params![session_id],
                    |row| row.get::<_, bool>(0),
                )
                .context("failed to inspect stale mailbox participant rows")?
        };
        if !has_departed_rows {
            return Ok(Vec::new());
        }

        // Reactivation is initiated by HTTP/MCP mailbox requests. Use the
        // bounded request admission path so contention returns the existing
        // retryable 503 instead of pinning a request thread indefinitely.
        let _write_guard = self.lock_writer("reactivating a mailbox participant")?;
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(Vec::new());
        };
        let departed_rows = {
            let mut statement = connection
                .prepare(
                    "SELECT mailbox_id, left_at
                     FROM mailbox_participants
                     WHERE session_id = ?1
                       AND left_at IS NOT NULL",
                )
                .context("failed to prepare stale mailbox participant lookup")?;
            statement
                .query_map(rusqlite::params![session_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .context("failed to inspect stale mailbox participant rows")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("failed to read stale mailbox participant rows")?
        };
        connection
            .execute(
                "UPDATE mailbox_participants
                 SET left_at = NULL
                 WHERE session_id = ?1
                   AND left_at IS NOT NULL",
                rusqlite::params![session_id],
            )
            .context("failed to reactivate stale mailbox participant rows")?;
        Ok(departed_rows)
    }

    /// Restores only participant rows cleared by one reactivation attempt.
    fn restore_reactivated_session_rows(
        &self,
        session_id: &str,
        rows: &[(String, String)],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let _write_guard = self.lock_writer("restoring mailbox participant markers")?;
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(());
        };
        for (mailbox_id, left_at) in rows {
            connection
                .execute(
                    "UPDATE mailbox_participants
                     SET left_at = ?3
                     WHERE mailbox_id = ?1
                       AND session_id = ?2
                       AND left_at IS NULL",
                    rusqlite::params![mailbox_id, session_id, left_at],
                )
                .with_context(|| {
                    format!(
                        "failed to restore mailbox participant marker for `{session_id}` in `{mailbox_id}`"
                    )
                })?;
        }
        Ok(())
    }

    fn list_for_session(&self, session_id: &str) -> Result<Vec<MailboxSummary>> {
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(Vec::new());
        };
        mailbox_summaries_for_session(&connection, session_id)
    }

    fn unread_wakeup_for_mailbox(
        &self,
        session_id: &str,
        mailbox_id: &str,
    ) -> Result<Option<MailboxUnreadWakeup>> {
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(None);
        };
        let result = connection.query_row(
            "SELECT message.id, message.sequence,
                    (
                      SELECT COUNT(*)
                      FROM mailbox_messages unread
                      WHERE unread.mailbox_id = mine.mailbox_id
                        AND unread.target_session_id = ?1
                        AND unread.sequence > mine.processed_through
                        AND COALESCE(unread.topic, '') != ?3
                    ),
                    message.sender_session_id, message.sender_name
             FROM mailbox_participants mine
             JOIN mailbox_messages message
               ON message.mailbox_id = mine.mailbox_id
              AND message.sequence = (
                SELECT MAX(candidate.sequence)
                FROM mailbox_messages candidate
                WHERE candidate.mailbox_id = mine.mailbox_id
                  AND candidate.sequence > mine.processed_through
                  AND candidate.target_session_id = ?1
                  AND COALESCE(candidate.topic, '') != ?3
              )
             WHERE mine.session_id = ?1
               AND mine.mailbox_id = ?2
               AND mine.left_at IS NULL",
            rusqlite::params![session_id, mailbox_id, DELEGATION_REVIEW_RESULT_TOPIC],
            |row| {
                Ok(MailboxUnreadWakeup {
                    mailbox_id: mailbox_id.to_owned(),
                    message_id: row.get(0)?,
                    sequence: row.get(1)?,
                    unread_count: row.get(2)?,
                    sender_session_id: row.get(3)?,
                    sender_name: row.get(4)?,
                })
            },
        );
        match result {
            Ok(wakeup) => Ok(Some(wakeup)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(anyhow!(err).context("failed to query current mailbox wake-up")),
        }
    }

    /// Reconstructs only the unread wake covered by one failed delivery.
    /// Newer mailbox rows must not be folded into this recovery because they
    /// may already belong to a successor active or queued turn.
    fn unread_wakeup_for_mailbox_through(
        &self,
        session_id: &str,
        mailbox_id: &str,
        through_sequence: u64,
    ) -> Result<Option<MailboxUnreadWakeup>> {
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(None);
        };
        let result = connection.query_row(
            "SELECT message.id, message.sequence,
                    (
                      SELECT COUNT(*)
                      FROM mailbox_messages unread
                      WHERE unread.mailbox_id = mine.mailbox_id
                        AND unread.target_session_id = ?1
                        AND unread.sequence > mine.processed_through
                        AND unread.sequence <= ?3
                        AND COALESCE(unread.topic, '') != ?4
                    ),
                    message.sender_session_id, message.sender_name
             FROM mailbox_participants mine
             JOIN mailbox_messages message
               ON message.mailbox_id = mine.mailbox_id
              AND message.sequence = (
                SELECT MAX(candidate.sequence)
                FROM mailbox_messages candidate
                WHERE candidate.mailbox_id = mine.mailbox_id
                  AND candidate.sequence > mine.processed_through
                  AND candidate.sequence <= ?3
                  AND candidate.target_session_id = ?1
                  AND COALESCE(candidate.topic, '') != ?4
              )
             WHERE mine.session_id = ?1
               AND mine.mailbox_id = ?2
               AND mine.left_at IS NULL",
            rusqlite::params![
                session_id,
                mailbox_id,
                through_sequence,
                DELEGATION_REVIEW_RESULT_TOPIC
            ],
            |row| {
                Ok(MailboxUnreadWakeup {
                    mailbox_id: mailbox_id.to_owned(),
                    message_id: row.get(0)?,
                    sequence: row.get(1)?,
                    unread_count: row.get(2)?,
                    sender_session_id: row.get(3)?,
                    sender_name: row.get(4)?,
                })
            },
        );
        match result {
            Ok(wakeup) => Ok(Some(wakeup)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(anyhow!(err).context("failed to query failed mailbox wake-up")),
        }
    }

    fn wakeups_for_session(
        &self,
        session_id: &str,
        recovery: MailboxWakeupRecovery,
    ) -> Result<Vec<MailboxUnreadWakeup>> {
        // Structured review results use mailbox durability as a result
        // transport, not as an edge-triggered prompt. Excluding that one
        // versioned topic keeps both ordinary and boot recovery from racing
        // the delegation wait that owns parent fan-in.
        let recovery_mode = match recovery {
            MailboxWakeupRecovery::NeverWoken => 0_i64,
            MailboxWakeupRecovery::AllUnreadAfterBoot => 1_i64,
        };
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(Vec::new());
        };
        let mut statement = connection
            .prepare(
                "SELECT m.id, message.id, message.sequence,
                        (
                          SELECT COUNT(*)
                          FROM mailbox_messages unread
                          WHERE unread.mailbox_id = m.id
                            AND unread.target_session_id = ?1
                            AND unread.sequence > mine.processed_through
                            AND COALESCE(unread.topic, '') != ?3
                        ),
                        message.sender_session_id, message.sender_name
                 FROM mailboxes m
                 JOIN mailbox_participants mine
                   ON mine.mailbox_id = m.id
                  AND mine.session_id = ?1
                  AND mine.left_at IS NULL
                 JOIN mailbox_messages message
                   ON message.mailbox_id = m.id
                  AND message.sequence = (
                    SELECT MAX(candidate.sequence)
                    FROM mailbox_messages candidate
                    WHERE candidate.mailbox_id = m.id
                      AND candidate.sequence > mine.processed_through
                      AND candidate.target_session_id = ?1
                      AND COALESCE(candidate.topic, '') != ?3
                      AND (
                        ?2 = 1
                        OR (?2 = 0 AND candidate.notification_disposition = 'durableButNotWoken')
                      )
                 )
                 WHERE message.sequence IS NOT NULL
                 ORDER BY message.created_at DESC, m.id
                 LIMIT CASE WHEN ?2 = 1 THEN -1 ELSE 16 END",
            )
            .context("failed to prepare unread mailbox wake-up query")?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    session_id,
                    recovery_mode,
                    DELEGATION_REVIEW_RESULT_TOPIC
                ],
                |row| {
                Ok(MailboxUnreadWakeup {
                    mailbox_id: row.get(0)?,
                    message_id: row.get(1)?,
                    sequence: row.get(2)?,
                    unread_count: row.get(3)?,
                    sender_session_id: row.get(4)?,
                    sender_name: row.get(5)?,
                })
                },
            )
            .context("failed to query unread mailbox wake-ups")?;
        rows.map(|row| row.context("failed to decode unread mailbox wake-up"))
            .collect()
    }

    #[cfg(test)]
    fn unread_wakeups_for_session(&self, session_id: &str) -> Result<Vec<MailboxUnreadWakeup>> {
        self.wakeups_for_session(session_id, MailboxWakeupRecovery::NeverWoken)
    }

    fn read_range(
        &self,
        session_id: &str,
        mailbox_id: &str,
        after_sequence: u64,
        limit: u64,
    ) -> Result<Vec<MailboxMessage>> {
        let limit = limit.clamp(1, 200);
        let connection = self.connection()?;
        require_mailbox_participant(&connection, mailbox_id, session_id)?;
        let mut statement = connection
            .prepare(
                "SELECT id, mailbox_id, sequence, sender_session_id, sender_name,
                        target_session_id, target_name, created_at, class, topic,
                        state_stamp, body, idempotency_key, unread_depth_at_append,
                        notification_disposition
                 FROM mailbox_messages
                 WHERE mailbox_id = ?1 AND sequence > ?2
                   AND NOT (
                     target_session_id = ?4
                     AND COALESCE(topic, '') = ?5
                   )
                 ORDER BY sequence
                 LIMIT ?3",
            )
            .context("failed to prepare mailbox range query")?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    mailbox_id,
                    after_sequence,
                    limit,
                    session_id,
                    DELEGATION_REVIEW_RESULT_TOPIC
                ],
                mailbox_message_from_row,
            )
            .context("failed to query mailbox messages")?;
        rows.map(|row| row.context("failed to decode mailbox message"))
            .collect()
    }

    fn read_message(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<MailboxMessage> {
        let connection = self.connection()?;
        let message = connection
            .query_row(
                "SELECT id, mailbox_id, sequence, sender_session_id, sender_name,
                        target_session_id, target_name, created_at, class, topic,
                        state_stamp, body, idempotency_key, unread_depth_at_append,
                        notification_disposition
                 FROM mailbox_messages
                 WHERE id = ?1
                   AND NOT (
                     target_session_id = ?2
                     AND COALESCE(topic, '') = ?3
                   )",
                rusqlite::params![
                    message_id,
                    session_id,
                    DELEGATION_REVIEW_RESULT_TOPIC
                ],
                mailbox_message_from_row,
            )
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => mailbox_store_error(
                    MailboxStoreErrorKind::NotFound,
                    "mailbox message not found",
                ),
                other => anyhow!(other),
            })?;
        require_mailbox_participant(&connection, &message.mailbox_id, session_id)?;
        Ok(message)
    }

    fn acknowledge(
        &self,
        session_id: &str,
        mailbox_id: &str,
        expected_processed_through: u64,
        processed_through: u64,
    ) -> Result<MailboxSummary> {
        if processed_through < expected_processed_through {
            return Err(mailbox_store_error(
                MailboxStoreErrorKind::Validation,
                "mailbox acknowledgement cannot move backwards",
            ));
        }
        let write_guard = self.lock_writer("waiting to begin mailbox acknowledgement")?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "beginning mailbox acknowledgement")?;
        require_mailbox_participant(&transaction, mailbox_id, session_id)?;
        let latest_sequence = transaction
            .query_row(
                "SELECT next_sequence - 1 FROM mailboxes WHERE id = ?1",
                rusqlite::params![mailbox_id],
                |row| row.get::<_, u64>(0),
            )
            .map_err(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => {
                    mailbox_store_error(MailboxStoreErrorKind::NotFound, "mailbox not found")
                }
                other => anyhow!(other),
            })?;
        if processed_through > latest_sequence {
            return Err(mailbox_store_error(
                MailboxStoreErrorKind::Validation,
                format!(
                    "mailbox acknowledgement {} exceeds latest sequence {}",
                    processed_through, latest_sequence
                ),
            ));
        }
        let updated = transaction
            .execute(
                "UPDATE mailbox_participants
                 SET processed_through = ?4
                 WHERE mailbox_id = ?1
                   AND session_id = ?2
                   AND left_at IS NULL
                   AND processed_through = ?3",
                rusqlite::params![
                    mailbox_id,
                    session_id,
                    expected_processed_through,
                    processed_through
                ],
            )
            .context("failed to update mailbox acknowledgement")?;
        if updated == 0 {
            let current_processed_through = transaction
                .query_row(
                    "SELECT processed_through
                     FROM mailbox_participants
                     WHERE mailbox_id = ?1
                       AND session_id = ?2
                       AND left_at IS NULL",
                    rusqlite::params![mailbox_id, session_id],
                    |row| row.get::<_, u64>(0),
                )
                .map_err(|err| match err {
                    rusqlite::Error::QueryReturnedNoRows => mailbox_store_error(
                        MailboxStoreErrorKind::NotFound,
                        "mailbox participant not found",
                    ),
                    other => anyhow!(other),
                })?;
            if current_processed_through < processed_through {
                return Err(mailbox_store_error(
                    MailboxStoreErrorKind::Conflict,
                    format!(
                        "mailbox acknowledgement conflict: processedThrough no longer equals {}",
                        expected_processed_through
                    ),
                ));
            }
        }
        let summary = mailbox_summaries_for_session(&transaction, session_id)?
            .into_iter()
            .find(|summary| summary.id == mailbox_id)
            .ok_or_else(|| {
                mailbox_store_error(
                    MailboxStoreErrorKind::NotFound,
                    "mailbox not found while acknowledging",
                )
            })?;
        transaction
            .commit()
            .map_err(|err| {
                mailbox_sqlite_write_error("committing mailbox acknowledgement", err)
            })?;
        drop(connection);
        drop(write_guard);
        Ok(summary)
    }
}

fn begin_mailbox_write<'connection>(
    connection: &'connection mut rusqlite::Connection,
    operation: &str,
) -> Result<rusqlite::Transaction<'connection>> {
    connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|err| mailbox_sqlite_write_error(operation, err))
}

fn mailbox_sqlite_write_error(operation: &str, err: rusqlite::Error) -> anyhow::Error {
    if matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    ) {
        return mailbox_store_error(
            MailboxStoreErrorKind::Retryable,
            format!(
                "mailbox storage is temporarily busy while {operation}; no mailbox write was \
                 committed by this operation, so retry the same request"
            ),
        );
    }
    anyhow!(err).context(format!("failed while {operation}"))
}

fn validate_mailbox_append_input(input: &MailboxAppendInput) -> Result<()> {
    if input.sender_session_id == input.target_session_id {
        return Err(mailbox_store_error(
            MailboxStoreErrorKind::Validation,
            "mailbox messages must target another session",
        ));
    }
    if input.sender_session_id.trim().is_empty()
        || input.target_session_id.trim().is_empty()
        || input.idempotency_key.trim().is_empty()
    {
        return Err(mailbox_store_error(
            MailboxStoreErrorKind::Validation,
            "mailbox sender, target, and idempotency key are required",
        ));
    }
    if input.body.trim().is_empty() {
        return Err(mailbox_store_error(
            MailboxStoreErrorKind::Validation,
            "mailbox message cannot be empty",
        ));
    }
    if input.idempotency_key.len() > 256 {
        return Err(mailbox_store_error(
            MailboxStoreErrorKind::Validation,
            "mailbox idempotency key exceeds 256 bytes",
        ));
    }
    if input.body.len() > MAX_MAILBOX_BODY_BYTES {
        return Err(mailbox_store_error(
            MailboxStoreErrorKind::Validation,
            format!("mailbox body exceeds {MAX_MAILBOX_BODY_BYTES} bytes"),
        ));
    }
    if input
        .topic
        .as_ref()
        .is_some_and(|topic| topic.len() > MAX_MAILBOX_METADATA_BYTES)
    {
        return Err(mailbox_store_error(
            MailboxStoreErrorKind::Validation,
            format!("mailbox topic exceeds {MAX_MAILBOX_METADATA_BYTES} bytes"),
        ));
    }
    if input
        .state_stamp
        .as_ref()
        .is_some_and(|state_stamp| state_stamp.len() > MAX_MAILBOX_METADATA_BYTES)
    {
        return Err(mailbox_store_error(
            MailboxStoreErrorKind::Validation,
            format!("mailbox state stamp exceeds {MAX_MAILBOX_METADATA_BYTES} bytes"),
        ));
    }
    Ok(())
}


fn mailbox_participant_key(left: &str, right: &str) -> String {
    let mut participants = [left, right];
    participants.sort_unstable();
    serde_json::to_string(&participants).expect("mailbox participant ids should serialize")
}

fn upsert_mailbox_participant(
    transaction: &rusqlite::Transaction<'_>,
    mailbox_id: &str,
    session_id: &str,
    display_name: &str,
    now: &str,
) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO mailbox_participants(
               mailbox_id, session_id, display_name, processed_through, joined_at, left_at
             ) VALUES(?1, ?2, ?3, 0, ?4, NULL)
             ON CONFLICT(mailbox_id, session_id) DO UPDATE SET
               display_name = excluded.display_name",
            rusqlite::params![mailbox_id, session_id, display_name, now],
        )
        .context("failed to upsert mailbox participant")?;
    Ok(())
}

fn mailbox_message_for_idempotency_key(
    connection: &rusqlite::Connection,
    sender_session_id: &str,
    idempotency_key: &str,
) -> Result<Option<MailboxIdempotencyRecord>> {
    match connection.query_row(
        "SELECT mailbox_id, id, sequence, sender_name, target_session_id,
                target_name, body, topic, state_stamp, unread_depth_at_append,
                dispatch_outcome
         FROM mailbox_messages
         WHERE sender_session_id = ?1 AND idempotency_key = ?2",
        rusqlite::params![sender_session_id, idempotency_key],
        |row| {
            Ok(MailboxIdempotencyRecord {
                mailbox_id: row.get(0)?,
                message_id: row.get(1)?,
                sequence: row.get(2)?,
                sender_name: row.get(3)?,
                target_session_id: row.get(4)?,
                target_name: row.get(5)?,
                body: row.get(6)?,
                topic: row.get(7)?,
                state_stamp: row.get(8)?,
                unread_depth_at_append: row.get(9)?,
                dispatch_outcome: row.get(10)?,
            })
        },
    ) {
        Ok(message) => Ok(Some(message)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err).context("failed to look up mailbox idempotency key"),
    }
}

fn mailbox_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailboxMessage> {
    Ok(MailboxMessage {
        id: row.get(0)?,
        mailbox_id: row.get(1)?,
        sequence: row.get(2)?,
        sender_session_id: row.get(3)?,
        sender_name: row.get(4)?,
        target_session_id: row.get(5)?,
        target_name: row.get(6)?,
        created_at: row.get(7)?,
        class: row.get(8)?,
        topic: row.get(9)?,
        state_stamp: row.get(10)?,
        body: row.get(11)?,
        idempotency_key: row.get(12)?,
        unread_depth_at_append: row.get(13)?,
        notification_state: row.get(14)?,
    })
}

fn mailbox_summaries_for_session(
    connection: &rusqlite::Connection,
    session_id: &str,
) -> Result<Vec<MailboxSummary>> {
    let mut statement = connection
        .prepare(
            "SELECT m.id, latest.sequence,
                    (
                      SELECT COUNT(*)
                      FROM mailbox_messages unread
                      WHERE unread.mailbox_id = m.id
                        AND unread.target_session_id = ?1
                        AND unread.sequence > mine.processed_through
                        AND COALESCE(unread.topic, '') != ?2
                    ),
                    latest.body, latest.created_at
             FROM mailboxes m
             JOIN mailbox_participants mine
               ON mine.mailbox_id = m.id AND mine.session_id = ?1
             JOIN mailbox_messages latest
               ON latest.mailbox_id = m.id
              AND latest.sequence = (
                SELECT MAX(candidate.sequence)
                FROM mailbox_messages candidate
                WHERE candidate.mailbox_id = m.id
                  AND COALESCE(candidate.topic, '') != ?2
              )
             WHERE mine.left_at IS NULL
             ORDER BY COALESCE(latest.created_at, m.created_at) DESC, m.id",
        )
        .context("failed to prepare mailbox summary query")?;
    let rows = statement
        .query_map(
            rusqlite::params![session_id, DELEGATION_REVIEW_RESULT_TOPIC],
            |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
            },
        )
        .context("failed to query mailbox summaries")?;
    let mut summaries = Vec::new();
    for row in rows {
        let (id, latest_sequence, unread_count, latest_body, latest_message_at) =
            row.context("failed to decode mailbox summary")?;
        summaries.push(MailboxSummary {
            participants: mailbox_participants(connection, &id)?,
            id,
            latest_sequence,
            unread_count,
            latest_message_preview: latest_body.map(|body| mailbox_preview(&body)),
            latest_message_at,
        });
    }
    Ok(summaries)
}

fn mailbox_participants(
    connection: &rusqlite::Connection,
    mailbox_id: &str,
) -> Result<Vec<MailboxParticipant>> {
    let mut statement = connection
        .prepare(
            "SELECT session_id, display_name, processed_through, left_at
             FROM mailbox_participants
             WHERE mailbox_id = ?1
             ORDER BY session_id",
        )
        .context("failed to prepare mailbox participant query")?;
    let rows = statement
        .query_map(rusqlite::params![mailbox_id], |row| {
            Ok(MailboxParticipant {
                session_id: row.get(0)?,
                display_name: row.get(1)?,
                processed_through: row.get(2)?,
                left_at: row.get(3)?,
            })
        })
        .context("failed to query mailbox participants")?;
    rows.map(|row| row.context("failed to decode mailbox participant"))
        .collect()
}

fn require_mailbox_participant(
    connection: &rusqlite::Connection,
    mailbox_id: &str,
    session_id: &str,
) -> Result<()> {
    let exists = connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM mailbox_participants
               WHERE mailbox_id = ?1 AND session_id = ?2 AND left_at IS NULL
             )",
            rusqlite::params![mailbox_id, session_id],
            |row| row.get::<_, bool>(0),
        )
        .context("failed to authorize mailbox participant")?;
    if !exists {
        return Err(mailbox_store_error(
            MailboxStoreErrorKind::NotFound,
            "mailbox not found for this session",
        ));
    }
    Ok(())
}

fn mailbox_preview(body: &str) -> String {
    const MAX_CHARS: usize = 160;
    let single_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = single_line.chars();
    let preview = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

#[cfg(test)]
#[path = "mailboxes_store_tests.rs"]
mod mailbox_store_tests;
