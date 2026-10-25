/*
Durable neutral mailbox storage.

Mailboxes are coordination records, not agent sessions: they have no runtime,
workdir, prompt queue, or model. SQLite is authoritative for message bodies and
participant cursors and issued read-page receipts. Reads record issuance but
never acknowledgement; both receipt and legacy numeric acknowledgements verify
issuance before advancing. This store owns one long-lived connection, independent of
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notification_cause: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notification_cause: Option<String>,
    mailbox_id: String,
    message_id: String,
    sequence: u64,
    unread_depth: u64,
    notification_disposition: String,
    duplicate: bool,
    sender_processed_through: u64,
    sender_cursor_advanced: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MailboxReadResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receipt_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    read_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    page_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cursor_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reader_session_id: Option<String>,
    messages: Vec<MailboxMessage>,
    after_sequence: u64,
    processed_through: u64,
    #[serde(default)]
    receipt: Option<String>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_after_sequence: Option<u64>,
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
    topic: Option<String>,
}

/// The queue head as dispatch-time revalidation saw it under the state lock.
#[derive(Clone, Debug)]
struct FrontMailboxHead {
    prompt_id: String,
    mailbox_id: String,
    message_id: String,
    sequence: u64,
    text: String,
    source: Option<MessageSource>,
}

/// One optimistic revalidation read: the head snapshot plus what the mailbox
/// store said about it off the state lock.
#[derive(Clone, Debug)]
struct FrontMailboxRevalidation {
    head: FrontMailboxHead,
    wakeup: Option<MailboxUnreadWakeup>,
    boundary: Option<MailboxWakeBoundary>,
}

#[derive(Default)]
struct MailboxWakeQueueOutcome {
    accepted: bool,
    changed: bool,
    relay_owner: Option<MailboxRelayDrainOwner>,
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
    Capacity,
}

#[derive(Debug)]
struct MailboxStoreError {
    kind: MailboxStoreErrorKind,
    message: String,
    // Only a lead writer-admission refusal may certify notCommitted. An
    // operation with an earlier commit must never reuse that certificate.
    writer_admission: bool,
}

impl std::fmt::Display for MailboxStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MailboxStoreError {}

fn mailbox_store_error(kind: MailboxStoreErrorKind, message: impl Into<String>) -> anyhow::Error {
    MailboxStoreError {
        kind,
        message: message.into(),
        writer_admission: false,
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
    read_key: Option<String>,
    // Only the agent MCP/CLI transport opts into durable page issuance.
    // Missing stays preview-safe for existing UI bundles and old bridges.
    #[serde(default)]
    issue_receipt: bool,
    #[serde(default)]
    after_sequence: Option<u64>,
    #[serde(default = "default_mailbox_read_limit")]
    limit: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcknowledgeMailboxRequest {
    receipt: Option<String>,
    expected_processed_through: Option<u64>,
    processed_through: Option<u64>,
    #[serde(default)]
    no_continuation: bool,
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
    path: PathBuf,
    federation_reads: Mutex<FederationMailboxReadCache>,
    federation_read_intents: Mutex<HashMap<String, FederationMailboxClientRead>>,
    /// Invalidates optimistic zero-unread coverage reads on a new append.
    continuation_epoch: AtomicU64,
    connection: Mutex<Option<rusqlite::Connection>>,
    write_lock: Arc<SqliteStateWriterAdmission>,
    write_admission_timeout: Duration,
    dispatch_finalization: Arc<MailboxDispatchFinalization>,
    /// Serializes the durable structured-review append with recovery's
    /// read/probe decision. The main AppState mutex is never held across this
    /// coordination-store I/O; a conditional attempt check protects the later
    /// in-memory probe commit from a concurrent delegation rearm.
    delegation_review_recovery_guard: Mutex<()>,
    executor_reservations: Mutex<FederationExecutorReservationCache>,
    // Live admission evidence bridges primary queue exposure and a failed
    // coordination marker write. Not handoff authority or a boot tombstone.
    relay_live_queued: Mutex<HashMap<(String, String), u64>>,
    #[cfg(test)]
    before_native_wake: Mutex<Option<Box<dyn FnOnce() + Send>>>,
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
fn decrement_dispatch_waiter(state: &mut MailboxDispatchFinalizationState, message_id: &str) {
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
    fn ensure_mailbox_session_active(&self, session_id: &str) -> std::result::Result<(), ApiError> {
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
        let creations = self.standalone_mailbox_creations([
            sender_session_id, &request.target_session_id,
        ])?;
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
            .append_authorized(&input, || self.authorize_standalone_mailbox(
                [sender_session_id, &input.target_session_id], &creations,
            ))
            .map_err(mailbox_api_error)?;
        self.finish_mailbox_append_and_notify(sender_session_id, &input.target_session_id, &input, appended)
    }

    /// The disposition of a mailbox wake whose dispatch was held before
    /// provider hand-off: waiting for the due automatic Engram retry that now
    /// owns its head, if one does; otherwise `None`, recorded as before.
    fn mailbox_held_wake_disposition(&self, target_session_id: &str) -> Option<&'static str> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(target_session_id)
            .map(|index| &inner.sessions[index])
            .and_then(mailbox_wake_disposition_behind_automatic_retry)
    }

    /// The disposition of a mailbox wake that was queued rather than started:
    /// behind a turn that is running, or held, when no turn runs and the
    /// session's queue is paused (a retained prompt, a Stop, or a failed
    /// mailbox turn). A held wake waits for that hold to clear, not for a
    /// turn to end; a hold that a due automatic Engram retry owns clears by
    /// itself and is named as that retry (`engram_retry_hold.rs`).
    fn mailbox_queued_wake_disposition(&self, target_session_id: &str) -> &'static str {
        let inner = self.inner.lock().expect("state mutex poisoned");
        if inner.find_session_index(target_session_id).is_some_and(|index|
            inner.sessions[index].provider_capacity_retry.as_ref().is_some_and(|retry| retry.due_at.is_some() || (retry.recovering && !retry.dispatched))) {
            return "waitingForProviderCapacity";
        }
        if let Some(disposition) = inner
            .find_session_index(target_session_id)
            .map(|index| &inner.sessions[index])
            .filter(|record| {
                !matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
                )
            })
            .and_then(mailbox_wake_disposition_behind_automatic_retry)
        {
            return disposition;
        }
        let held = inner
            .find_session_index(target_session_id)
            .map(|index| &inner.sessions[index])
            .is_some_and(|record| {
                record.orchestrator_auto_dispatch_blocked
                    && !matches!(
                        record.session.status,
                        SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
                    )
            });
        if held {
            "heldBehindPausedQueue"
        } else {
            "queuedBehindActiveTurn"
        }
    }

    fn mailbox_peer_names(
        &self,
        sender_session_id: &str,
        target_session_id: &str,
    ) -> std::result::Result<(String, String), ApiError> {
        if !matches!(mailbox_reference_handle(target_session_id), Ok(None)) {
            return Err(ApiError::not_found("mailbox reference requires explicit resolution"));
        }
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
            inner.find_session_index(session_id).is_some_and(|index| {
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
        let Some(storage_key)=self.local_mailbox_storage_key(session_id) else { return Ok(false); };
        let wakeups = self
            .mailbox_store
            .wakeups_for_session(&storage_key, recovery)?;
        self.queue_mailbox_wakeups_for_session(session_id, wakeups, recovery)
    }

    fn requeue_rejected_mailbox_notification(
        &self,
        notification: &MailboxNotificationDelivery,
    ) -> Result<bool> {
        self.requeue_mailbox_notification_with_rejection(notification, false)
    }

    fn requeue_mailbox_notification_with_rejection(
        &self, notification: &MailboxNotificationDelivery, known_rejected: bool,
    ) -> Result<bool> {
        if MailboxRelayLocator::decode(&notification.mailbox_id,false).map_err(|error|anyhow!(error.message))?.is_some() {
            return self.requeue_native_mailbox_relay_notification(notification, known_rejected);
        }
        let Some(storage_key)=self.local_mailbox_storage_key(&notification.session_id) else { return Ok(false); };
        let Some(wakeup) = self.mailbox_store.unread_wakeup_for_mailbox_through(
            &storage_key,
            &notification.mailbox_id,
            notification.through_sequence,
        )?
        else {
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
                &storage_key,
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
        self.queue_mailbox_wakeups_for_binding_outcome(
            session_id, wakeups, recovery, promote_existing_to_front, None,
        )
    }

    fn queue_mailbox_wakeups_for_binding_outcome(
        &self,
        session_id: &str,
        wakeups: Vec<MailboxUnreadWakeup>,
        recovery: MailboxWakeupRecovery,
        promote_existing_to_front: bool,
        expected_binding: Option<&FederationMailboxBinding>,
    ) -> Result<MailboxWakeQueueOutcome> {
        self.queue_mailbox_wakeups_for_native_outcome(session_id,wakeups,recovery,promote_existing_to_front,expected_binding,None)
    }

    fn queue_mailbox_wakeups_for_native_outcome(
        &self, session_id:&str, wakeups:Vec<MailboxUnreadWakeup>, recovery:MailboxWakeupRecovery,
        promote_existing_to_front:bool, expected_binding:Option<&FederationMailboxBinding>,
        expected_relay:Option<(&MailboxRelayPrincipal,Option<&FederationLinkLease>, &FederationPeerSnapshot)>,
    ) -> Result<MailboxWakeQueueOutcome> {
        let storage_key = if let Some((principal,_,_))=expected_relay { principal.participant_key() }
            else { let Some(key)=self.local_mailbox_storage_key(session_id) else { return Ok(MailboxWakeQueueOutcome::default()); }; key };
        if wakeups.is_empty() {
            return Ok(MailboxWakeQueueOutcome::default());
        }

        let relay_link = expected_relay.and_then(|(_,lease,_)|lease).map(|lease| {
            let guard = lease.channel.state.lock().expect("federation link mutex poisoned");
            if guard.incarnation.as_deref() != Some(lease.incarnation.as_str())
                || guard.recorded_incarnation.as_deref() != Some(lease.incarnation.as_str()) {
                return Err(relay_store_error("native mailbox notification link changed"));
            }
            Ok(guard)
        }).transpose()?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_visible_session_index(session_id) else {
            return Ok(MailboxWakeQueueOutcome::default());
        };
        if let Some((principal,lease,peer)) = expected_relay {
            let current = federation_snapshot_locked(&inner, &peer.peer.host_id).map_err(|error|relay_store_error(&error.message))?;
            if current.membership != FederationMembershipState::Active || !same_mailbox_relay_enrollment(peer, &current) {
                return Err(relay_store_error("native relay wake enrollment changed before queue admission"));
            }
            if !inner.preferences.federation.enabled || principal.native_session_id != session_id || inner.federation.local.as_ref() != Some(&principal.host)
                || inner.sessions[index].federation_native_creation_id != principal.native_creation_id {
                return Ok(MailboxWakeQueueOutcome::default());
            }
            if let Some(lease) = lease {
                self.refresh_federation_transport_snapshot_locked(&inner,&lease.channel.transport_basis(),&lease.incarnation)
                    .map_err(|error|relay_store_error(&error.message))?;
            }
        }
        if let Some(binding) = expected_binding {
            // Revalidate while applying the queue change, after all store I/O.
            // A delayed committed wake never targets a recreated native root.
            if inner.sessions[index].federation_mailbox_binding.as_ref() != Some(binding)
                || inner.federation.mailbox_directory.get(&storage_key).is_none_or(|row| {
                    row.binding != *binding
                        || !federation_mailbox_participant_current_locked(&inner, row)
                })
            {
                return Ok(MailboxWakeQueueOutcome::default());
            }
        }
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
            let text = if wakeup.unread_count == 0 { mailbox_continuation_notification_text(
                &wakeup.mailbox_id,wakeup.sequence,&wakeup.sender_name,wakeup.topic.as_deref(),
            ) } else { mailbox_notification_text(
                &wakeup.mailbox_id,
                wakeup.unread_count,
                wakeup.sequence,
                &wakeup.sender_name,
                wakeup.topic.as_deref(),
            ) };
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
            // A retained request is immutable, but it still covers its original
            // boundary. Recovery of that same wake must not append a duplicate.
            if record.queued_prompts.iter().any(|queued| {
                queued.is_engram_retained()
                    && queued
                        .pending_prompt
                        .source
                        .as_ref()
                        .and_then(|source| source.mailbox.as_ref())
                        .is_some_and(|mailbox| {
                            mailbox.mailbox_id == wakeup.mailbox_id
                                && mailbox.sequence >= wakeup.sequence
                        })
            }) {
                continue;
            }
            // An entry enqueued with a durability prerequisite keeps the
            // content proven durable; it is never rewritten as a wake.
            let durability_holds = &record.queued_prompt_durability_holds;
            if let Some(existing_index) = record.queued_prompts.iter().position(|queued| {
                !queued.is_engram_retained()
                    && !durability_holds.contains_key(&queued.pending_prompt.id)
                    && queued
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
                // Like an Engram-retained head, a head held for its
                // durability acknowledgement is never overtaken.
                if promote_existing_to_front
                    && !record
                        .queued_prompts
                        .front()
                        .is_some_and(QueuedPromptRecord::is_engram_retained)
                    && !queued_head_held_for_durability(record)
                {
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
            let insert_at = usize::from(
                record
                    .queued_prompts
                    .front()
                    .is_some_and(QueuedPromptRecord::is_engram_retained)
                    || queued_head_held_for_durability(record),
            );
            record.queued_prompts.insert(
                insert_at,
                QueuedPromptRecord {
                    engram_waiting: false,
                    promoted_message_index: None,
                    promotion_disposition_known: true,
                    engram_bind: None,
                    engram_evaluate: None,
                    engram_interrupted: false,
                    source: QueuedPromptSource::Mailbox,
                    attachments: Vec::new(),
                    pending_prompt: PendingPrompt {
                        engram_interrupted: false,
                        is_engram_retained: false,
                        attachments: Vec::new(),
                        id: prompt_id,
                        timestamp: stamp_now(),
                        text,
                        expanded_text: None,
                        source: Some(source),
                    },
                },
            );
            changed = true;
        }
        let mut relay_owner = None;
        if let Some((principal, _, _)) = expected_relay {
            for (mailbox, sequence) in &recovered_through {
                let Some(locator) = MailboxRelayLocator::decode(mailbox, false)
                    .map_err(|error|anyhow!(error.message))? else { continue; };
                let record = &inner.sessions[index];
                let covered = record.queued_prompts.iter().any(|queued|
                    queued.pending_prompt.source.as_ref().and_then(|source|source.mailbox.as_ref())
                        .is_some_and(|source|source.mailbox_id == *mailbox && source.sequence >= *sequence));
                if covered {
                    // Record before releasing the same lock that first exposes
                    // the cancellable carrier, even if either later write fails.
                    self.mailbox_store.note_live_mailbox_relay_queued(principal, &locator.id, *sequence);
                    relay_owner = relay_owner.or_else(|| MailboxRelayDrainOwner::capture_boundary(
                        record, &locator.home, &locator.id, *sequence, self.federation_executor_wake_signal.clone()));
                }
            }
        }
        if changed {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            sync_pending_prompts(record);
            self.commit_locked(&mut inner)?;
        }
        drop(inner);
        drop(relay_link);
        for (mailbox_id, sequence) in recovered_through {
            if expected_relay.is_some() { continue; }
            self.mailbox_store.mark_notifications_recovered_through(
                &storage_key,
                &mailbox_id,
                sequence,
                recovery,
            )?;
        }
        Ok(MailboxWakeQueueOutcome {
            accepted: true,
            changed,
            relay_owner,
        })
    }

    fn reconcile_never_woken_mailbox_notifications_for_session(
        &self,
        session_id: &str,
    ) -> Result<bool> {
        self.reconcile_mailbox_wakeups_for_session(session_id, MailboxWakeupRecovery::NeverWoken)
    }

    /// Revalidates the queue head against the durable participant cursor.
    ///
    /// A mailbox wake can sit behind a long-running turn while the receiver
    /// independently reads and acknowledges the same mailbox. Acknowledgement
    /// advances the data cursor only: an accepted wake whose mail is all
    /// acknowledged stays as the continuation turn, with text that claims no
    /// unread mail. It is dropped only when its boundary row is no longer
    /// visible or the row's delivery record shows a handoff already covered
    /// it. Returns `true` when the queue head changed and the caller should
    /// inspect it again.
    fn revalidate_front_mailbox_wakeup_for_session(&self, session_id: &str) -> Result<bool> {
        let Some(read) = self.read_front_mailbox_revalidation(session_id)? else {
            return Ok(false);
        };
        self.apply_front_mailbox_revalidation(session_id, read)
    }

    /// Snapshots the queue head under the state lock, then reads its mailbox
    /// off the lock. Never hold the mailbox connection mutex together with
    /// StateInner.
    fn read_front_mailbox_revalidation(
        &self,
        session_id: &str,
    ) -> Result<Option<FrontMailboxRevalidation>> {
        let Some(head) = ({
            let inner = self.inner.lock().expect("state mutex poisoned");
            // An entry enqueued with a durability prerequisite keeps the
            // content proven durable: revalidation never rewrites or drops it.
            inner
                .find_session_index(session_id)
                .map(|index| &inner.sessions[index])
                .and_then(|record| {
                    record.queued_prompts.front().filter(|queued| {
                        !queued.is_engram_retained()
                            && !queued_prompt_kept_standalone(record, queued)
                    })
                })
                .and_then(|queued| {
                    let mailbox = queued.pending_prompt.source.as_ref()?.mailbox.as_ref()?;
                    Some(FrontMailboxHead {
                        prompt_id: queued.pending_prompt.id.clone(),
                        mailbox_id: mailbox.mailbox_id.clone(),
                        message_id: mailbox.message_id.clone(),
                        sequence: mailbox.sequence,
                        text: queued.pending_prompt.text.clone(),
                        source: queued.pending_prompt.source.clone(),
                    })
                })
        }) else {
            return Ok(None);
        };
        if let Some(locator) = MailboxRelayLocator::decode(&head.mailbox_id,false).map_err(|error|anyhow!(error.message))? {
            return self.read_native_mailbox_relay_revalidation(session_id,head,&locator);
        }
        let Some(storage_key)=self.local_mailbox_storage_key(session_id) else { return Ok(None); };
        let wakeup = self
            .mailbox_store
            .unread_wakeup_for_mailbox(&storage_key, &head.mailbox_id)?;
        let boundary = match wakeup {
            Some(_) => None,
            None => self.mailbox_store.mailbox_wake_boundary(
                &storage_key,
                &head.mailbox_id,
                &head.message_id,
            )?,
        };
        Ok(Some(FrontMailboxRevalidation {
            head,
            wakeup,
            boundary,
        }))
    }

    /// Applies an off-lock read to the queue head it was taken from.
    ///
    /// The read is optimistic. It applies only while the head is unchanged
    /// since the snapshot: same prompt, text and source. A send or an
    /// acknowledgement that rewrote the head in the meantime keeps the head's
    /// identity and boundary, so identity alone cannot tell; any such change
    /// makes the caller re-inspect instead of writing the older snapshot back.
    fn apply_front_mailbox_revalidation(
        &self,
        session_id: &str,
        read: FrontMailboxRevalidation,
    ) -> Result<bool> {
        let FrontMailboxRevalidation {
            head,
            wakeup,
            boundary,
        } = read;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(false);
        };
        let front_matches = inner.sessions[index]
            .queued_prompts
            .front()
            .is_some_and(|queued| {
                queued.pending_prompt.id == head.prompt_id
                    && queued.pending_prompt.text == head.text
                    && queued.pending_prompt.source == head.source
            });
        if !front_matches {
            return Ok(true);
        }
        let FrontMailboxHead {
            mailbox_id,
            message_id: head_message_id,
            sequence: head_sequence,
            ..
        } = head;
        // Authorization may have been prepared during the off-lock mailbox
        // read. Its text/source is now replay input, not a coalescible wake.
        // The same holds for an entry that took a durability hold meanwhile.
        if inner.sessions[index]
            .queued_prompts
            .front()
            .is_some_and(|queued| {
                queued.is_engram_retained()
                    || queued_prompt_kept_standalone(&inner.sessions[index], queued)
            })
        {
            return Ok(false);
        }

        let Some(wakeup) = wakeup else {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            let queued = record
                .queued_prompts
                .front_mut()
                .expect("validated mailbox queue head should exist");
            let Some(boundary) = boundary.filter(|boundary| !boundary.delivered) else {
                record.queued_prompts.pop_front();
                sync_pending_prompts(record);
                self.commit_locked(&mut inner)?;
                return Ok(true);
            };
            let changed = rewrite_queued_wake_as_continuation(
                queued,
                &mailbox_id,
                &head_message_id,
                head_sequence,
                boundary,
            );
            if changed || queued.source != QueuedPromptSource::Mailbox {
                queued.source = QueuedPromptSource::Mailbox;
                sync_pending_prompts(record);
                self.commit_locked(&mut inner)?;
            }
            return Ok(false);
        };

        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let queued = record
            .queued_prompts
            .front_mut()
            .expect("validated mailbox queue head should exist");
        let text = mailbox_notification_text(
            &wakeup.mailbox_id,
            wakeup.unread_count,
            wakeup.sequence,
            &wakeup.sender_name,
            wakeup.topic.as_deref(),
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

    /// Revalidate mailbox heads without making their side channel an executor.
    ///
    /// Ordinary mailbox failures are best-effort. A relay carrier stays held
    /// on unavailable authority; its typed hold is nonfatal to generic drains.
    fn revalidate_queued_mailbox_wakeups_before_dispatch(&self, session_id: &str) -> Result<()> {
        if let Err(error) = self.settle_covered_mailbox_continuations(session_id, None) {
            eprintln!("mailbox coverage> deferred at drain: {error:#}");
        }
        loop {
            match self.revalidate_front_mailbox_wakeup_for_session(session_id) {
                Ok(true) => continue,
                Ok(false) => return Ok(()),
                Err(err) => {
                    eprintln!(
                        "mailbox> failed revalidating queued notification for `{session_id}`: {err:#}"
                    );
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    if inner.find_session_index(session_id).and_then(|index|inner.sessions[index].queued_prompts.front())
                        .and_then(|queued|queued.pending_prompt.source.as_ref()).and_then(|source|source.mailbox.as_ref())
                        .is_some_and(|source|source.mailbox_id.starts_with("relay-mailbox-")) { return Err(mailbox_relay_admission_held(err)); }
                    return Ok(());
                }
            }
        }
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

    fn mark_mailbox_notification_delivered(&self, notification: &MailboxNotificationDelivery) {
        if matches!(MailboxRelayLocator::decode(&notification.mailbox_id,false),Ok(Some(_))) {
            if let Err(error) = self.mark_native_mailbox_relay_handoff(notification) {
                eprintln!("native mailbox relay handoff metadata remains unsettled: {error:#}");
            }
            return;
        }
        let Some(storage_key) = self.local_mailbox_storage_key(&notification.session_id) else {
            return;
        };
        if let Err(err) = self.mailbox_store.mark_notifications_delivered_through(
            &storage_key,
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
    topic: Option<&str>,
) -> String {
    // Display metadata is bounded and single-line; durable fields stay intact.
    // The protocol is taught by tools/help and managed instructions, not wakes.
    let sender_name = mailbox_preview(sender_name);
    let topic_line = topic.map_or(String::new(), |topic| {
        format!("Topic: {}\n", mailbox_preview(topic))
    });
    format!(
        "[TermAl mailbox notification]\n\
         Mailbox `{mailbox_id}` has {unread_count} unread message(s). Latest inbound: #{sequence} from {sender_name}.\n\
         {topic_line}Read: `termal_read_mailbox`; acknowledge after processing."
    )
}

async fn send_mailbox_message(
    AxumPath(sender_session_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<SendMailboxApiRequest>,
) -> Result<(StatusCode, Json<MailboxAppendReceipt>), ApiError> {
    let request = request.message;
    if let Some(receipt)=state.send_native_mailbox_relay(&sender_session_id,&request).await? {
        return Ok((StatusCode::ACCEPTED,Json(receipt)));
    }
    if !matches!(mailbox_reference_handle(&request.target_session_id), Ok(None)) {
        return Err(ApiError::not_found("mailbox reference requires explicit resolution"));
    }
    if state.federation_mailbox_actor_if_bound(&sender_session_id)?.is_some() {
        if request.class.as_deref().is_some_and(|class| class != "routine") {
            return Err(ApiError::bad_request("durable mailboxes currently support only class `routine`"));
        }
        let receipt = state.federation_mailbox_operation(&sender_session_id,FederationMailboxOperation::Send {
            target:request.target_session_id,message:request.message,key:request.idempotency_key,
            topic:request.topic,state_stamp:request.state_stamp }).await?;
        return Ok((StatusCode::ACCEPTED,Json(receipt)));
    }
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
    let Json(request) =
        request.map_err(|rejection| api_json_rejection("delegation review result", rejection))?;
    let receipt =
        run_blocking_api(move || state.submit_delegation_review_result(&child_session_id, request))
            .await?;
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}

#[cfg(test)]
async fn list_mailboxes(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<MailboxSummary>>, ApiError> {
    Ok(Json(list_mailboxes_with_availability(&session_id, &state).await?.mailboxes))
}

async fn list_mailboxes_api(
    AxumPath(session_id): AxumPath<String>, State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let listing = list_mailboxes_with_availability(&session_id, &state).await?;
    if query.get("includeRelayAvailability").is_some_and(|value|value == "true") {
        return Ok(Json(listing).into_response());
    }
    let unavailable = listing.unavailable_relay_homes.len();
    let unavailable_domain = listing.unavailable_central_domain.is_some();
    let mut response = Json(listing.mailboxes).into_response();
    if unavailable > 0 {
        response.headers_mut().insert("x-termal-unavailable-relay-homes",
            unavailable.to_string().parse().map_err(|_|ApiError::internal("invalid relay availability header"))?);
    }
    if unavailable_domain {
        response.headers_mut().insert("x-termal-unavailable-central-domain", axum::http::HeaderValue::from_static("1"));
    }
    if unavailable > 0 || unavailable_domain {
        response.headers_mut().insert("link", axum::http::HeaderValue::from_static("<?includeRelayAvailability=true>; rel=\"mailbox-availability\""));
    }
    Ok(response)
}

async fn list_mailboxes_with_availability(session_id: &str, state: &AppState) -> Result<MailboxRelayListing, ApiError> {
    let mut relays=state.list_native_mailbox_relays(session_id).await?;
    let bound_central = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let binding = inner.find_session_index(session_id)
            .and_then(|index| inner.sessions[index].federation_mailbox_binding.as_ref());
        if binding.is_some() {
            federation_mailbox_root_locked(&inner, session_id)?;
        }
        binding.map(|binding| binding.central.clone())
    };
    if let Some(central) = bound_central {
        let domain:Result<Vec<MailboxSummary>,ApiError>=state.federation_mailbox_operation(session_id,FederationMailboxOperation::List).await;
        match domain {
            Ok(mut summaries) => {
                summaries.append(&mut relays.mailboxes);
                relays.mailboxes = summaries;
            }
            Err(error) if !relays.mailboxes.is_empty() || !relays.unavailable_relay_homes.is_empty() => {
                // A domain binding is not authority over separate relay history.
                // Preserve its refusal explicitly, with no standalone fallback,
                // invented domain summary or weakened domain authorization.
                relays.unavailable_central_domain = Some(MailboxRelayUnavailableDomain {
                    central, availability:"unavailable_unverified", status:error.status.as_u16(), error:error.message,
                });
            }
            Err(error) => return Err(error), // no relay history: preserve the legacy domain refusal
        }
        return Ok(relays);
    }
    let state = state.clone();
    let session_id = session_id.to_owned();
    let mut summaries = run_blocking_api(move || {
        state.ensure_mailbox_session_active(&session_id)?;
        state
            .mailbox_store
            .list_for_session(&session_id)
            .map_err(mailbox_api_error)
    })
    .await?;
    summaries.append(&mut relays.mailboxes);
    relays.mailboxes = summaries;
    Ok(relays)
}

async fn read_mailbox(
    AxumPath((session_id, mailbox_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<ReadMailboxRequest>,
) -> Result<Json<MailboxReadResponse>, ApiError> {
    if let Some(locator)=MailboxRelayLocator::decode(&mailbox_id,false)? {
        return state.read_native_mailbox_relay(&session_id,locator,request).await.map(Json);
    }
    if state.federation_mailbox_actor_if_bound(&session_id)?.is_some() {
        return state.read_federation_mailbox(&session_id, mailbox_id, request.after_sequence,
            request.limit, request.issue_receipt, request.read_key).await.map(Json);
    }
    if request.read_key.is_some() {
        return Err(ApiError::conflict("readKey requires its original central mailbox binding; no local fallback"));
    }
    let messages = run_blocking_api(move || {
        state.ensure_mailbox_session_active(&session_id)?;
        let turn = state.mailbox_coverage_turn(&session_id);
        let page = state
            .mailbox_store
            .read_range_with_issuance(
                &session_id,
                &mailbox_id,
                request.after_sequence,
                request.limit,
                request.issue_receipt,
            )
            .map_err(mailbox_api_error)?;
        if request.issue_receipt {
            state.observe_mailbox_coverage_read(&session_id, &mailbox_id, turn, &page);
        }
        Ok(page)
    })
    .await?;
    Ok(Json(messages))
}

async fn read_mailbox_message(
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<MailboxMessage>, ApiError> {
    if let Some(locator)=MailboxRelayLocator::decode(&message_id,true)? {
        return state.read_native_mailbox_relay_message(&session_id,locator).await.map(Json);
    }
    if state.federation_mailbox_actor_if_bound(&session_id)?.is_some() {
        return state.federation_mailbox_operation(&session_id,FederationMailboxOperation::ReadMessage { message_id }).await.map(Json);
    }
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
    if let Some(locator)=MailboxRelayLocator::decode(&mailbox_id,false)? {
        return state.acknowledge_native_mailbox_relay(&session_id,locator,request).await.map(Json);
    }
    if state.federation_mailbox_actor_if_bound(&session_id)?.is_some() {
        if request.no_continuation {
            return Err(ApiError::bad_request(
                "noContinuation is unavailable for central-domain mailboxes; acknowledge the receipt without it and keep the continuation",
            ));
        }
        let (Some(receipt),None,None) = (request.receipt,request.expected_processed_through,request.processed_through) else {
            return Err(ApiError::bad_request("central domain acknowledgement requires only the unchanged issued receipt"));
        };
        return state.federation_mailbox_operation(&session_id,FederationMailboxOperation::Acknowledge { mailbox_id,receipt }).await.map(Json);
    }
    let summary = run_blocking_api(move || {
        state.ensure_mailbox_session_active(&session_id)?;
        if request.no_continuation && request.receipt.is_none() {
            return Err(ApiError::bad_request(
                "noContinuation requires a receipt acknowledgement",
            ));
        }
        let turn = state.mailbox_coverage_turn(&session_id);
        match (
            request.receipt,
            request.expected_processed_through,
            request.processed_through,
        ) {
            (Some(receipt), None, None) => {
                let (summary, receipt_through) = state
                    .mailbox_store
                    .acknowledge_page_with_declaration(
                        &session_id,
                        &mailbox_id,
                        &receipt,
                        request.no_continuation.then(|| turn.as_ref().map_or(0, |t| t.generation)),
                    )
                    .map_err(mailbox_api_error)?;
                state.observe_mailbox_coverage_ack(
                    &session_id,
                    &mailbox_id,
                    turn,
                    &receipt,
                    receipt_through,
                    request.no_continuation,
                );
                let through = summary
                    .participants
                    .iter()
                    .find(|participant| participant.session_id == session_id)
                    .map(|participant| participant.processed_through)
                    .unwrap_or(0);
                if let Err(err) =
                    state.refresh_acknowledged_mailbox_wakeups(&session_id, &mailbox_id, through)
                {
                    eprintln!("mailbox> receipt committed but wake refresh failed: {err:#}");
                }
                Ok(summary)
            }
            (None, Some(expected), Some(through)) => state
                .acknowledge_mailbox_and_refresh_covered_wakeups(
                    &session_id,
                    &mailbox_id,
                    expected,
                    through,
                )
                .map_err(mailbox_api_error),
            _ => Err(ApiError::bad_request(
                "Supply receipt OR both expectedProcessedThrough and processedThrough",
            )),
        }
    })
    .await?;
    Ok(Json(summary))
}

fn mailbox_api_error(err: anyhow::Error) -> ApiError {
    if let Some(mailbox_error) = err.downcast_ref::<MailboxStoreError>() {
        if mailbox_error.writer_admission {
            return ApiError::from_status(StatusCode::SERVICE_UNAVAILABLE,
                mailbox_error.message.clone())
                .with_kind(ApiErrorKind::FederationMailboxWriterAdmission);
        }
        return match mailbox_error.kind {
            MailboxStoreErrorKind::Validation => {
                ApiError::bad_request(mailbox_error.message.clone())
            }
            MailboxStoreErrorKind::Conflict => ApiError::conflict(mailbox_error.message.clone()),
            MailboxStoreErrorKind::NotFound => ApiError::not_found(mailbox_error.message.clone()),
            MailboxStoreErrorKind::Retryable => ApiError::from_status(
                StatusCode::SERVICE_UNAVAILABLE,
                mailbox_error.message.clone(),
            ),
            MailboxStoreErrorKind::Capacity => ApiError::from_status(
                StatusCode::TOO_MANY_REQUESTS, mailbox_error.message.clone()),
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
            path: path.to_path_buf(),
            federation_reads: Mutex::new(FederationMailboxReadCache::default()),
            federation_read_intents: Mutex::new(HashMap::new()),
            connection: Mutex::new(Some(connection)),
            continuation_epoch: AtomicU64::new(0),
            write_lock: sqlite_state_write_lock(path),
            write_admission_timeout,
            dispatch_finalization: Arc::new(MailboxDispatchFinalization::default()),
            delegation_review_recovery_guard: Mutex::new(()),
            executor_reservations: Mutex::new(FederationExecutorReservationCache::default()),
            relay_live_queued: Mutex::new(HashMap::new()),
            #[cfg(test)]
            before_native_wake: Mutex::new(None),
            #[cfg(test)]
            internal_writer_waiters: Mutex::new(0),
            #[cfg(test)]
            internal_writer_waiter_changed: Condvar::new(),
        })
    }

    #[cfg(test)]
    fn disabled_for_tests() -> Self {
        Self {
            path: PathBuf::new(),
            federation_reads: Mutex::new(FederationMailboxReadCache::default()),
            federation_read_intents: Mutex::new(HashMap::new()),
            connection: Mutex::new(None),
            continuation_epoch: AtomicU64::new(0),
            write_lock: Arc::new(SqliteStateWriterAdmission::default()),
            write_admission_timeout: MAILBOX_WRITER_ADMISSION_TIMEOUT,
            dispatch_finalization: Arc::new(MailboxDispatchFinalization::default()),
            delegation_review_recovery_guard: Mutex::new(()),
            executor_reservations: Mutex::new(FederationExecutorReservationCache::default()),
            relay_live_queued: Mutex::new(HashMap::new()),
            before_native_wake: Mutex::new(None),
            internal_writer_waiters: Mutex::new(0),
            internal_writer_waiter_changed: Condvar::new(),
        }
    }

    fn lock_writer(&self, operation: &str) -> Result<SqliteStateWriterGuard<'_>> {
        self.lock_writer_with_budget(operation, self.write_admission_timeout)
    }

    fn lock_writer_with_budget(&self, operation: &str, budget: Duration) -> Result<SqliteStateWriterGuard<'_>> {
        lock_sqlite_state_writer_for(&self.write_lock, self.write_admission_timeout.min(budget)).ok_or_else(
            || {
                anyhow::Error::new(MailboxStoreError {
                    kind: MailboxStoreErrorKind::Retryable,
                    writer_admission: true,
                    message: format!(
                        "mailbox storage is temporarily busy while {operation}; no mailbox write \
                         was committed by this operation, so retry the same request"
                    ),
                })
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
                let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now())
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
                other => {
                    anyhow!(other).context("failed to read finalized mailbox dispatch outcome")
                }
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
        guard.is_some().then_some(MailboxConnectionGuard { guard })
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
        self.append_authorized(input, || Ok(()))
    }

    fn append_authorized<G>(&self, input: &MailboxAppendInput, authorize: impl FnOnce() -> Result<G>) -> Result<MailboxAppendResult> {
        self.append_with_delivery(input, None, authorize)
    }

    fn append_with_delivery<G>(&self, input: &MailboxAppendInput, delivery: Option<&FederationDelivery>, authorize: impl FnOnce() -> Result<G>) -> Result<MailboxAppendResult> {
        self.append_with_delivery_and_relay(input, delivery, None, authorize)
    }

    fn append_with_delivery_and_relay<G>(&self, input: &MailboxAppendInput, delivery: Option<&FederationDelivery>, relay: Option<(&MailboxRelayAppend, &FederationPeerSnapshot)>, authorize: impl FnOnce() -> Result<G>) -> Result<MailboxAppendResult> {
        validate_mailbox_append_input(input)?;
        if let Some((relay, _))=relay {
            if relay.input().map_err(|error|relay_store_error(&error.message))? != *input {
                return Err(relay_store_error("native relay append input changed"));
            }
        }
        let write_guard = if delivery.is_some() || relay.is_some() {
            self.lock_writer_with_budget("waiting to begin mailbox append", Duration::from_secs(2))?
        } else { self.lock_writer("waiting to begin mailbox append")? };
        let mut connection = self.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "beginning mailbox append")?;
        if relay.is_none() && transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM mailbox_relay_intents WHERE owner_session_id=?1 AND idempotency_key=?2)",
            rusqlite::params![input.sender_session_id,input.idempotency_key],|row|row.get::<_,bool>(0))? {
            return Err(relay_store_error("idempotency key belongs to a native relay send; no standalone fallback"));
        }
        if let Some((relay, _)) = relay { check_mailbox_relay_home(&transaction, relay)?; }
        if let Some(delivery) = delivery {
            federation_delivery_check_central(&transaction, delivery)?;
        }

        if let Some(existing) = mailbox_message_for_idempotency_key(
            &transaction,
            &input.sender_session_id,
            &input.idempotency_key,
        )? {
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
            let sender_processed_through = mailbox_processed_through(
                &transaction,
                &existing.mailbox_id,
                &input.sender_session_id,
            )?;
            if let Some(delivery) = delivery {
                federation_delivery_commit_central(&transaction, delivery, &existing.message_id, existing.sequence)?;
            }
            if let Some((relay, peer)) = relay {
                commit_mailbox_relay_append(&transaction, relay, peer, &existing.mailbox_id, &existing.message_id, existing.sequence, existing.unread_depth_at_append)?;
            }
            let authority = authorize()?;
            transaction.commit().map_err(|err| {
                mailbox_sqlite_write_error("finishing duplicate mailbox lookup", err)
            })?;
            // Lock order: a duplicate may wait on dispatch finalization only after
            // releasing both SQLite resources and the shared writer-admission guard.
            drop(authority);
            drop(connection);
            drop(write_guard);
            let notification_disposition = match existing.dispatch_outcome {
                // Domain receipts describe commitment, not the later wake.
                _ if delivery.is_some() || relay.is_some() => "durableButNotWoken".to_owned(),
                Some(dispatch_outcome) => dispatch_outcome,
                None => self.wait_for_final_dispatch_outcome(&existing.message_id)?,
            };
            return Ok(MailboxAppendResult {
                receipt: MailboxAppendReceipt {
                    notification_cause: None,
                    mailbox_id: existing.mailbox_id,
                    message_id: existing.message_id,
                    sequence: existing.sequence,
                    unread_depth: existing.unread_depth_at_append,
                    notification_disposition,
                    duplicate: true,
                    sender_processed_through,
                    sender_cursor_advanced: false,
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
                rusqlite::params![&mailbox_id, &input.target_session_id, processed_through],
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
        let sender_cursor_advanced = transaction
            .execute(
                "UPDATE mailbox_participants
                 SET processed_through = ?3
                 WHERE mailbox_id = ?1
                   AND session_id = ?2
                   AND left_at IS NULL
                   AND processed_through = ?3 - 1",
                rusqlite::params![&mailbox_id, &input.sender_session_id, sequence],
            )
            .context("failed to advance sender mailbox cursor")?
            == 1;
        // Prepare the receipt in the append transaction: no fallible cursor
        // lookup after commit, and no claim that this snapshot fences later sends.
        let sender_processed_through =
            mailbox_processed_through(&transaction, &mailbox_id, &input.sender_session_id)?;
        if let Some(delivery) = delivery {
            federation_delivery_commit_central(&transaction, delivery, &message_id, sequence)?;
            #[cfg(test)]
            federation_delivery_test_cut("beforeCentralCommit");
        }
        if let Some((relay, peer)) = relay {
            commit_mailbox_relay_append(&transaction, relay, peer, &mailbox_id, &message_id, sequence, unread_depth)?;
        }
        let authority = authorize()?;
        self.register_pending_dispatch_outcome(&message_id);
        let finalization = MailboxDispatchFinalizationGuard {
            dispatch_finalization: self.dispatch_finalization.clone(),
            message_id: message_id.clone(),
        };
        if let Err(err) = transaction.commit() {
            return Err(mailbox_sqlite_write_error("committing mailbox append", err));
        }
        self.continuation_epoch.fetch_add(1, Ordering::SeqCst);
        drop(authority);
        drop(write_guard);

        Ok(MailboxAppendResult {
            receipt: MailboxAppendReceipt {
                notification_cause: None,
                mailbox_id,
                message_id,
                sequence,
                unread_depth,
                notification_disposition: notification_disposition.to_owned(),
                duplicate: false,
                sender_processed_through,
                sender_cursor_advanced,
            },
            finalization: Some(finalization),
        })
    }

    fn record_initial_dispatch_outcome(
        &self,
        message_id: &str,
        dispatch_outcome: &str,
    ) -> Result<MailboxDispatchOutcomeRecord> {
        self.record_initial_dispatch_outcome_advancing(message_id, dispatch_outcome, true)
    }

    /// Records the receipt's immutable `dispatch_outcome` only, leaving the
    /// row's wake lifecycle state `durableButNotWoken`, so the never-woken
    /// recovery still covers it: a wake held before hand-off is not woken,
    /// whatever its receipt names as the reason (`engram_retry_hold.rs`).
    fn record_held_dispatch_outcome(
        &self,
        message_id: &str,
        dispatch_outcome: &str,
    ) -> Result<MailboxDispatchOutcomeRecord> {
        self.record_initial_dispatch_outcome_advancing(message_id, dispatch_outcome, false)
    }

    fn record_initial_dispatch_outcome_advancing(
        &self,
        message_id: &str,
        dispatch_outcome: &str,
        advance_state: bool,
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
                transaction.commit().map_err(|err| {
                    mailbox_sqlite_write_error(
                        "committing duplicate mailbox dispatch finalization",
                        err,
                    )
                })?;
                return Ok(MailboxDispatchOutcomeRecord::AlreadyFinalized {
                    dispatch_outcome: existing_outcome,
                });
            }
            let state_advanced = if advance_state {
                transaction
                    .execute(
                        "UPDATE mailbox_messages
                         SET notification_disposition = ?2
                         WHERE id = ?1
                           AND notification_disposition = 'durableButNotWoken'",
                        rusqlite::params![message_id, dispatch_outcome],
                    )
                    .context("failed to advance initial mailbox notification state")?
            } else {
                0
            };
            transaction.commit().map_err(|err| {
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
                    message.sender_session_id, message.sender_name, message.topic
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
                    topic: row.get(5)?,
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
                    message.sender_session_id, message.sender_name, message.topic
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
                    topic: row.get(5)?,
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
                        message.sender_session_id, message.sender_name, message.topic
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
                rusqlite::params![session_id, recovery_mode, DELEGATION_REVIEW_RESULT_TOPIC],
                |row| {
                    Ok(MailboxUnreadWakeup {
                        mailbox_id: row.get(0)?,
                        message_id: row.get(1)?,
                        sequence: row.get(2)?,
                        unread_count: row.get(3)?,
                        sender_session_id: row.get(4)?,
                        sender_name: row.get(5)?,
                        topic: row.get(6)?,
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

    #[cfg(test)]
    fn read_range(
        &self,
        session_id: &str,
        mailbox_id: &str,
        after_sequence: Option<u64>,
        limit: u64,
    ) -> Result<MailboxReadResponse> {
        self.read_range_with_issuance(session_id, mailbox_id, after_sequence, limit, true)
    }

    fn read_range_with_issuance(
        &self,
        session_id: &str,
        mailbox_id: &str,
        after_sequence: Option<u64>,
        limit: u64,
        issue_receipt: bool,
    ) -> Result<MailboxReadResponse> {
        self.read_range_authorized(session_id, mailbox_id, after_sequence, limit, issue_receipt, || Ok(()))
    }

    fn read_range_authorized<G>(
        &self, session_id: &str, mailbox_id: &str, after_sequence: Option<u64>,
        limit: u64, issue_receipt: bool, authorize: impl FnOnce() -> Result<G>,
    ) -> Result<MailboxReadResponse> {
        let limit = limit.clamp(1, 200);
        let _write_guard = if issue_receipt {
            Some(self.lock_writer("recording mailbox page issuance")?)
        } else {
            None
        };
        let mut connection = self.connection()?;
        // Snapshot authorization, cursor, and rows together, including when
        // another store connection acknowledges concurrently.
        let transaction = if issue_receipt {
            begin_mailbox_write(&mut connection, "recording mailbox page issuance")?
        } else {
            connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)
                .context("failed to begin mailbox preview snapshot")?
        };
        require_mailbox_participant(&transaction, mailbox_id, session_id)?;
        let processed_through = mailbox_processed_through(&transaction, mailbox_id, session_id)?;
        let after_sequence = after_sequence.unwrap_or(processed_through);
        let mut statement = transaction
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
                    limit + 1,
                    session_id,
                    DELEGATION_REVIEW_RESULT_TOPIC
                ],
                mailbox_message_from_row,
            )
            .context("failed to query mailbox messages")?;
        let mut messages = rows
            .map(|row| row.context("failed to decode mailbox message"))
            .collect::<Result<Vec<_>>>()?;
        drop(statement);
        let has_more = messages.len() as u64 > limit;
        messages.truncate(limit as usize);
        let next_after_sequence = messages.last().map(|message| message.sequence);
        let receipt = if let Some(through) = next_after_sequence.filter(|_| issue_receipt) {
            let candidate = uuid::Uuid::new_v4().to_string();
            transaction.execute(
                "INSERT INTO mailbox_read_pages(receipt, mailbox_id, session_id, after_sequence, through_sequence)
                 VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(mailbox_id, session_id, after_sequence, through_sequence) DO NOTHING",
                rusqlite::params![candidate, mailbox_id, session_id, after_sequence, through],
            )?;
            Some(transaction.query_row(
                "SELECT receipt FROM mailbox_read_pages WHERE mailbox_id = ?1 AND session_id = ?2
                 AND after_sequence = ?3 AND through_sequence = ?4",
                rusqlite::params![mailbox_id, session_id, after_sequence, through],
                |row| row.get(0),
            )?)
        } else {
            None
        };
        let _authority = authorize()?;
        transaction
            .commit()
            .map_err(|err| mailbox_sqlite_write_error("committing mailbox page issuance", err))?;
        Ok(MailboxReadResponse {
            receipt_state: None, read_key: None, page_id: None, cursor_generation: None,
            reader_session_id: None,
            messages,
            after_sequence,
            processed_through,
            receipt,
            has_more,
            next_after_sequence,
        })
    }

    fn read_message(&self, session_id: &str, message_id: &str) -> Result<MailboxMessage> {
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
                rusqlite::params![message_id, session_id, DELEGATION_REVIEW_RESULT_TOPIC],
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
        self.acknowledge_validated(
            session_id,
            mailbox_id,
            None,
            expected_processed_through,
            processed_through,
            None,
        )
        .map(|r| r.0)
    }

    fn acknowledge_page(
        &self,
        session_id: &str,
        mailbox_id: &str,
        receipt: &str,
    ) -> Result<MailboxSummary> {
        self.acknowledge_validated(session_id, mailbox_id, Some(receipt), 0, 0, None)
            .map(|r| r.0)
    }

    fn acknowledge_page_with_declaration(
        &self,
        session_id: &str,
        mailbox_id: &str,
        receipt: &str,
        generation: Option<u64>,
    ) -> Result<(MailboxSummary, u64)> {
        self.acknowledge_validated(session_id, mailbox_id, Some(receipt), 0, 0, generation)
    }

    fn acknowledge_validated(
        &self,
        session_id: &str,
        mailbox_id: &str,
        receipt: Option<&str>,
        expected_processed_through: u64,
        processed_through: u64,
        declaration_generation: Option<u64>,
    ) -> Result<(MailboxSummary, u64)> {
        self.acknowledge_authorized_with_declaration(
            session_id, mailbox_id, receipt, expected_processed_through,
            processed_through, declaration_generation, || Ok(()),
        )
    }

    fn acknowledge_authorized<G>(
        &self, session_id: &str, mailbox_id: &str, receipt: Option<&str>,
        expected_processed_through: u64, processed_through: u64,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<MailboxSummary> {
        self.acknowledge_authorized_with_declaration(
            session_id, mailbox_id, receipt, expected_processed_through,
            processed_through, None, authorize,
        ).map(|result| result.0)
    }

    fn acknowledge_authorized_with_declaration<G>(
        &self,
        session_id: &str,
        mailbox_id: &str,
        receipt: Option<&str>,
        mut expected_processed_through: u64,
        mut processed_through: u64,
        declaration_generation: Option<u64>,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<(MailboxSummary, u64)> {
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
        let current = mailbox_processed_through(&transaction, mailbox_id, session_id)?;
        let mut receipt_through = None;
        if let Some(receipt) = receipt {
            let page = transaction.query_row(
                "SELECT after_sequence, through_sequence FROM mailbox_read_pages
                 WHERE receipt = ?1 AND mailbox_id = ?2 AND session_id = ?3",
                rusqlite::params![receipt, mailbox_id, session_id],
                |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?)),
            ).optional()?.ok_or_else(|| mailbox_store_error(
                MailboxStoreErrorKind::Validation, "mailbox receipt was not issued to this participant and mailbox; read again",
            ))?;
            // A later page must not acknowledge an earlier unprocessed visible
            // page, even if both have been read. Hidden review rows are not gaps.
            if page.1 > current
                && transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM mailbox_messages WHERE mailbox_id = ?1
                 AND sequence > ?2 AND sequence <= ?3
                 AND NOT (target_session_id = ?4 AND COALESCE(topic, '') = ?5))",
                    rusqlite::params![
                        mailbox_id,
                        current,
                        page.0,
                        session_id,
                        DELEGATION_REVIEW_RESULT_TOPIC
                    ],
                    |row| row.get::<_, bool>(0),
                )?
            {
                return Err(mailbox_store_error(
                    MailboxStoreErrorKind::Conflict,
                    "mailbox receipt skips an unacknowledged page; read from the current cursor and process it first",
                ));
            }
            expected_processed_through = current;
            processed_through = page.1.max(current);
            receipt_through = Some(page.1);
        }
        let latest_sequence: u64 = transaction.query_row(
            "SELECT next_sequence - 1 FROM mailboxes WHERE id = ?1",
            rusqlite::params![mailbox_id],
            |row| row.get(0),
        )?;
        if processed_through > latest_sequence {
            return Err(mailbox_store_error(
                MailboxStoreErrorKind::Validation,
                format!(
                    "mailbox acknowledgement {} exceeds latest sequence {}",
                    processed_through, latest_sequence
                ),
            ));
        }
        if processed_through > current {
            if expected_processed_through != current {
                return Err(mailbox_store_error(
                    MailboxStoreErrorKind::Conflict,
                    "mailbox acknowledgement conflict: cursor changed; read again",
                ));
            }
            // Numeric compatibility is NOT a bypass. Every visible row in the
            // advance must have been issued; already processed history needs no
            // backfill at upgrade. Own messages remain visible, hidden review
            // payloads do not. Issuance records never certify comprehension.
            let unissued = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM mailbox_messages m WHERE m.mailbox_id = ?1
                 AND m.sequence > ?2 AND m.sequence <= ?3
                 AND NOT (m.target_session_id = ?4 AND COALESCE(m.topic, '') = ?5)
                 AND NOT EXISTS(SELECT 1 FROM mailbox_read_pages p
                     WHERE p.mailbox_id = m.mailbox_id AND p.session_id = ?4
                     AND p.after_sequence < m.sequence
                     AND p.through_sequence >= m.sequence))",
                rusqlite::params![
                    mailbox_id,
                    current,
                    processed_through,
                    session_id,
                    DELEGATION_REVIEW_RESULT_TOPIC
                ],
                |row| row.get::<_, bool>(0),
            )?;
            if unissued {
                return Err(mailbox_store_error(
                    MailboxStoreErrorKind::Conflict,
                    "mailbox acknowledgement includes messages not issued to this participant; read from the current cursor using the current MCP/CLI bridge, process, then acknowledge. Old bridges without issueReceipt only preview: upgrade/restart the bridge or use the current CLI",
                ));
            }
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
        if let (Some(receipt), Some(through), Some(generation)) =
            (receipt, receipt_through, declaration_generation)
        {
            transaction.execute(
                "INSERT INTO mailbox_continuation_declarations(session_id, mailbox_id, receipt, through_sequence, turn_generation, declared_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(session_id, mailbox_id, receipt) DO UPDATE SET
                   turn_generation=excluded.turn_generation, declared_at=excluded.declared_at",
                rusqlite::params![session_id, mailbox_id, receipt, through, generation, stamp_now()],
            )?;
        }
        let _authority = authorize()?;
        transaction
            .commit()
            .map_err(|err| mailbox_sqlite_write_error("committing mailbox acknowledgement", err))?;
        drop(connection);
        drop(write_guard);
        Ok((summary, receipt_through.unwrap_or(processed_through)))
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
        notification_cause: None,
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
    mailbox_summaries_for_session_with_empty(connection, session_id, false)
}

fn mailbox_summaries_for_session_with_empty(
    connection: &rusqlite::Connection,
    session_id: &str,
    include_empty: bool,
) -> Result<Vec<MailboxSummary>> {
    let mut statement = connection
        .prepare(
            "SELECT m.id, COALESCE(latest.sequence, 0),
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
             LEFT JOIN mailbox_messages latest
               ON latest.mailbox_id = m.id
              AND latest.sequence = (
                SELECT MAX(candidate.sequence)
                FROM mailbox_messages candidate
                WHERE candidate.mailbox_id = m.id
                  AND COALESCE(candidate.topic, '') != ?2
              )
             WHERE mine.left_at IS NULL AND (?3 OR latest.id IS NOT NULL)
             ORDER BY COALESCE(latest.created_at, m.created_at) DESC, m.id",
        )
        .context("failed to prepare mailbox summary query")?;
    let rows = statement
        .query_map(
            rusqlite::params![session_id, DELEGATION_REVIEW_RESULT_TOPIC, include_empty],
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

fn mailbox_processed_through(
    connection: &rusqlite::Connection,
    mailbox_id: &str,
    session_id: &str,
) -> Result<u64> {
    connection
        .query_row(
            "SELECT processed_through FROM mailbox_participants
             WHERE mailbox_id = ?1 AND session_id = ?2",
            rusqlite::params![mailbox_id, session_id],
            |row| row.get(0),
        )
        .context("failed to read mailbox participant cursor")
}

fn mailbox_preview(body: &str) -> String {
    const MAX_CHARS: usize = 160;
    // Normalize whitespace before dropping controls so line/tab boundaries still
    // separate words. Remove invisible direction/format marks from display only.
    let display_text: String = body
        .chars()
        .map(|character| {
            if character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .filter(|character| {
            !character.is_control()
                && !matches!(
                    character,
                    '\u{200b}'..='\u{200f}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2060}'..='\u{2064}'
                        | '\u{2066}'..='\u{2069}'
                        | '\u{feff}'
                )
        })
        .collect();
    let single_line = display_text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
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
