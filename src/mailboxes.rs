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
    notification_disposition: String,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct MailboxUnreadWakeup {
    mailbox_id: String,
    message_id: String,
    sequence: u64,
    unread_count: u64,
    sender_session_id: String,
    sender_name: String,
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

struct MailboxStore {
    connection: Mutex<Option<rusqlite::Connection>>,
}

struct MailboxConnectionGuard<'a> {
    guard: std::sync::MutexGuard<'a, Option<rusqlite::Connection>>,
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
        let mut receipt = self
            .mailbox_store
            .append(&input)
            .map_err(mailbox_api_error)?;
        if receipt.duplicate {
            return Ok(receipt);
        }

        let (sender_still_active, target_still_active) =
            self.mailbox_participants_still_active(
                sender_session_id,
                &input.target_session_id,
            );
        for (session_id, still_active) in [
            (sender_session_id, sender_still_active),
            (input.target_session_id.as_str(), target_still_active),
        ] {
            if !still_active {
                self.mailbox_store
                    .mark_session_left(session_id)
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "mailbox message committed but failed to preserve deleted participant state: {err:#}"
                        ))
                    })?;
            }
        }
        if !target_still_active {
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
                if deliver_turn_dispatch(self, dispatch).is_ok() {
                    Some("deliveredToIdleSession")
                } else {
                    None
                }
            }
            Ok(DispatchTurnResult::DispatchedAfterQueue(dispatch)) => {
                if deliver_turn_dispatch(self, dispatch).is_ok() {
                    Some("queuedBehindActiveTurn")
                } else {
                    None
                }
            }
            Ok(DispatchTurnResult::Queued) => Some("queuedBehindActiveTurn"),
            Err(_) => None,
        };
        if let Some(disposition) = disposition {
            match self
                .mailbox_store
                .set_notification_disposition(&receipt.message_id, disposition)
            {
                Ok(()) => {
                    receipt.notification_disposition = disposition.to_owned();
                }
                Err(err) => {
                    eprintln!(
                        "mailbox> failed recording `{disposition}` for message `{}`: {err:#}",
                        receipt.message_id
                    );
                }
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
                || record.is_remote_proxy()
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
                        && !record.is_remote_proxy()
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
        if wakeups.is_empty() {
            return Ok(false);
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_visible_session_index(session_id) else {
            return Ok(false);
        };
        if inner.sessions[index].is_remote_proxy()
            || inner.sessions[index].session.parent_delegation_id.is_some()
            || inner
                .find_delegation_index_by_child_session_id(session_id)
                .is_some()
        {
            return Ok(false);
        }

        let mut changed = false;
        let mut recovered_through = Vec::with_capacity(wakeups.len());
        for wakeup in wakeups {
            recovered_through.push((
                wakeup.mailbox_id.clone(),
                wakeup.message_id.clone(),
                wakeup.sequence,
            ));
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
            if let Some(existing) = record.queued_prompts.iter_mut().find(|queued| {
                queued
                    .pending_prompt
                    .source
                    .as_ref()
                    .and_then(|candidate| candidate.mailbox.as_ref())
                    .is_some_and(|mailbox| mailbox.mailbox_id == wakeup.mailbox_id)
            }) {
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
                    continue;
                }
                if existing.pending_prompt.text != text
                    || existing.pending_prompt.source.as_ref() != Some(&source)
                {
                    existing.pending_prompt.timestamp = stamp_now();
                    existing.pending_prompt.text = text;
                    existing.pending_prompt.source = Some(source);
                    changed = true;
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
        for (mailbox_id, message_id, sequence) in recovered_through {
            self.mailbox_store.mark_notifications_recovered_through(
                session_id,
                &mailbox_id,
                &message_id,
                sequence,
            )?;
        }
        Ok(changed)
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
                        && !record.is_remote_proxy()
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
         `expectedProcessedThrough`."
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

async fn list_mailboxes(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<MailboxSummary>>, ApiError> {
    let summaries = run_blocking_api(move || {
        {
            let inner = state.inner.lock().expect("state mutex poisoned");
            if inner.find_visible_session_index(&session_id).is_none() {
                return Err(ApiError::not_found("session not found"));
            }
        }
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
        };
    }
    ApiError::internal(format!("mailbox operation failed: {err:#}"))
}

impl MailboxStore {
    fn open(path: &FsPath) -> Result<Self> {
        let connection = open_sqlite_state_connection(path)?;
        ensure_sqlite_state_schema(&connection)?;
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
        })
    }

    #[cfg(test)]
    fn disabled_for_tests() -> Self {
        Self {
            connection: Mutex::new(None),
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

    fn append(&self, input: &MailboxAppendInput) -> Result<MailboxAppendReceipt> {
        validate_mailbox_append_input(input)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("failed to begin mailbox append transaction")?;

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
                .context("failed to finish duplicate mailbox lookup")?;
            return Ok(MailboxAppendReceipt {
                mailbox_id: existing.mailbox_id,
                message_id: existing.id,
                sequence: existing.sequence,
                unread_depth: existing.unread_depth_at_append,
                notification_disposition: existing.notification_disposition,
                duplicate: true,
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
                   notification_disposition
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'routine', ?9, ?10, ?11, ?12, ?13, ?14)",
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
        transaction
            .commit()
            .context("failed to commit mailbox append")?;

        Ok(MailboxAppendReceipt {
            mailbox_id,
            message_id,
            sequence,
            unread_depth,
            notification_disposition: notification_disposition.to_owned(),
            duplicate: false,
        })
    }

    fn set_notification_disposition(
        &self,
        message_id: &str,
        disposition: &str,
    ) -> Result<()> {
        let connection = self.connection()?;
        let updated = connection
            .execute(
                "UPDATE mailbox_messages
                 SET notification_disposition = ?2
                 WHERE id = ?1",
                rusqlite::params![message_id, disposition],
            )
            .context("failed to update mailbox notification disposition")?;
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
    ) -> Result<()> {
        let connection = self.connection()?;
        connection
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
        Ok(())
    }

    fn mark_notifications_recovered_through(
        &self,
        session_id: &str,
        mailbox_id: &str,
        latest_message_id: &str,
        through_sequence: u64,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("failed to begin mailbox recovery disposition update")?;
        transaction
            .execute(
                "UPDATE mailbox_messages
                 SET notification_disposition = 'recoveredWake'
                 WHERE mailbox_id = ?1
                   AND target_session_id = ?2
                   AND sequence <= ?3
                   AND notification_disposition = 'durableButNotWoken'",
                rusqlite::params![mailbox_id, session_id, through_sequence],
            )
            .context("failed to mark never-woken mailbox notifications recovered")?;
        transaction
            .execute(
                "UPDATE mailbox_messages
                 SET notification_disposition = 'recoveredWake'
                 WHERE id = ?1
                   AND mailbox_id = ?2
                   AND target_session_id = ?3",
                rusqlite::params![latest_message_id, mailbox_id, session_id],
            )
            .context("failed to mark latest mailbox notification recovered")?;
        transaction
            .commit()
            .context("failed to commit mailbox recovery disposition update")?;
        Ok(())
    }

    fn mark_session_left(&self, session_id: &str) -> Result<()> {
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

    fn list_for_session(&self, session_id: &str) -> Result<Vec<MailboxSummary>> {
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(Vec::new());
        };
        let mut statement = connection
            .prepare(
                "SELECT m.id, m.next_sequence - 1,
                        (
                          SELECT COUNT(*)
                          FROM mailbox_messages unread
                          WHERE unread.mailbox_id = m.id
                            AND unread.target_session_id = ?1
                            AND unread.sequence > mine.processed_through
                        ),
                        latest.body, latest.created_at
                 FROM mailboxes m
                 JOIN mailbox_participants mine
                   ON mine.mailbox_id = m.id AND mine.session_id = ?1
                 LEFT JOIN mailbox_messages latest
                   ON latest.mailbox_id = m.id
                  AND latest.sequence = m.next_sequence - 1
                 WHERE mine.left_at IS NULL
                 ORDER BY COALESCE(latest.created_at, m.created_at) DESC, m.id",
            )
            .context("failed to prepare mailbox summary query")?;
        let rows = statement
            .query_map(rusqlite::params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u64>(1)?,
                    row.get::<_, u64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .context("failed to query mailbox summaries")?;
        let mut summaries = Vec::new();
        for row in rows {
            let (id, latest_sequence, unread_count, latest_body, latest_message_at) =
                row.context("failed to decode mailbox summary")?;
            summaries.push(MailboxSummary {
                participants: mailbox_participants(&connection, &id)?,
                id,
                latest_sequence,
                unread_count,
                latest_message_preview: latest_body.map(|body| mailbox_preview(&body)),
                latest_message_at,
            });
        }
        Ok(summaries)
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
              )
             WHERE mine.session_id = ?1
               AND mine.mailbox_id = ?2
               AND mine.left_at IS NULL",
            rusqlite::params![session_id, mailbox_id],
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

    fn wakeups_for_session(
        &self,
        session_id: &str,
        recovery: MailboxWakeupRecovery,
    ) -> Result<Vec<MailboxUnreadWakeup>> {
        let include_all_unread =
            i64::from(recovery == MailboxWakeupRecovery::AllUnreadAfterBoot);
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
                      AND (
                        ?2 = 1
                        OR candidate.notification_disposition = 'durableButNotWoken'
                      )
                 )
                 WHERE message.sequence IS NOT NULL
                 ORDER BY message.created_at DESC, m.id
                 LIMIT 16",
            )
            .context("failed to prepare unread mailbox wake-up query")?;
        let rows = statement
            .query_map(rusqlite::params![session_id, include_all_unread], |row| {
                Ok(MailboxUnreadWakeup {
                    mailbox_id: row.get(0)?,
                    message_id: row.get(1)?,
                    sequence: row.get(2)?,
                    unread_count: row.get(3)?,
                    sender_session_id: row.get(4)?,
                    sender_name: row.get(5)?,
                })
            })
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
                 ORDER BY sequence
                 LIMIT ?3",
            )
            .context("failed to prepare mailbox range query")?;
        let rows = statement
            .query_map(
                rusqlite::params![mailbox_id, after_sequence, limit],
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
                 WHERE id = ?1",
                rusqlite::params![message_id],
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
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("failed to begin mailbox acknowledgement")?;
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
            return Err(mailbox_store_error(
                MailboxStoreErrorKind::Conflict,
                format!(
                    "mailbox acknowledgement conflict: processedThrough no longer equals {}",
                    expected_processed_through
                ),
            ));
        }
        transaction
            .commit()
            .context("failed to commit mailbox acknowledgement")?;
        drop(connection);
        self.list_for_session(session_id)?
            .into_iter()
            .find(|summary| summary.id == mailbox_id)
            .ok_or_else(|| {
                mailbox_store_error(
                    MailboxStoreErrorKind::NotFound,
                    "mailbox not found after acknowledgement",
                )
            })
    }
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
    transaction: &rusqlite::Transaction<'_>,
    sender_session_id: &str,
    idempotency_key: &str,
) -> Result<Option<MailboxMessage>> {
    match transaction.query_row(
        "SELECT id, mailbox_id, sequence, sender_session_id, sender_name,
                target_session_id, target_name, created_at, class, topic,
                state_stamp, body, idempotency_key, unread_depth_at_append,
                notification_disposition
         FROM mailbox_messages
         WHERE sender_session_id = ?1 AND idempotency_key = ?2",
        rusqlite::params![sender_session_id, idempotency_key],
        mailbox_message_from_row,
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
        notification_disposition: row.get(14)?,
    })
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
mod mailbox_store_tests {
    use super::*;

    struct MailboxTestRoot(PathBuf);

    impl MailboxTestRoot {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("termal-mailbox-test-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).expect("mailbox test root should exist");
            Self(path)
        }

        fn database_path(&self) -> PathBuf {
            self.0.join("termal.sqlite")
        }
    }

    impl Drop for MailboxTestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn test_input() -> MailboxAppendInput {
        MailboxAppendInput {
            sender_session_id: "session-sender".to_owned(),
            sender_name: "Sender".to_owned(),
            target_session_id: "session-target".to_owned(),
            target_name: "Target".to_owned(),
            body: "Durable hello".to_owned(),
            idempotency_key: "send-1".to_owned(),
            topic: Some("coordination".to_owned()),
            state_stamp: Some("rev-7".to_owned()),
        }
    }

    #[test]
    fn mailbox_api_status_uses_typed_error_kind_instead_of_message_text() {
        let internal = mailbox_api_error(anyhow!(
            "internal database lookup reported not found and exceeds retry budget"
        ));
        assert_eq!(
            internal.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal wording must never be mistaken for a client classification"
        );

        let not_found = mailbox_api_error(mailbox_store_error(
            MailboxStoreErrorKind::NotFound,
            "mailbox not found",
        ));
        assert_eq!(not_found.status, StatusCode::NOT_FOUND);
        let conflict = mailbox_api_error(mailbox_store_error(
            MailboxStoreErrorKind::Conflict,
            "mailbox cursor conflict",
        ));
        assert_eq!(conflict.status, StatusCode::CONFLICT);
        let validation = mailbox_api_error(mailbox_store_error(
            MailboxStoreErrorKind::Validation,
            "mailbox input exceeds limit",
        ));
        assert_eq!(validation.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn append_retry_after_reopen_returns_original_durable_receipt() {
        let root = MailboxTestRoot::new();
        let path = root.database_path();

        let first = {
            let store = MailboxStore::open(&path).expect("mailbox store should open");
            store.append(&test_input()).expect("append should succeed")
        };
        assert!(!first.duplicate);
        assert_eq!(first.notification_disposition, "durableButNotWoken");

        let store = MailboxStore::open(&path).expect("mailbox store should reopen");
        store
            .acknowledge("session-target", &first.mailbox_id, 0, 1)
            .expect("target cursor should advance before retry");
        let duplicate = store
            .append(&test_input())
            .expect("idempotent retry should succeed");
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.mailbox_id, first.mailbox_id);
        assert_eq!(duplicate.message_id, first.message_id);
        assert_eq!(duplicate.sequence, first.sequence);
        assert_eq!(
            duplicate.unread_depth, first.unread_depth,
            "duplicate must return the original receipt, not recompute depth from the current cursor"
        );
        assert_eq!(duplicate.notification_disposition, "durableButNotWoken");
        assert_eq!(
            store
                .read_range("session-target", &first.mailbox_id, 0, 20)
                .expect("messages should read")
                .len(),
            1
        );
    }

    #[test]
    fn idempotent_retry_ignores_mutable_participant_display_names() {
        let root = MailboxTestRoot::new();
        let path = root.database_path();
        let store = MailboxStore::open(&path).expect("mailbox store should open");
        let first = store.append(&test_input()).expect("append should succeed");

        let mut renamed = test_input();
        renamed.sender_name = "Renamed Sender".to_owned();
        renamed.target_name = "Renamed Target".to_owned();
        let duplicate = store
            .append(&renamed)
            .expect("renaming either participant must not change message intent");
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.message_id, first.message_id);

        let stored = store
            .read_message("session-target", &first.message_id)
            .expect("original durable message should read");
        assert_eq!(stored.sender_name, "Sender");
        assert_eq!(stored.target_name, "Target");
    }

    #[test]
    fn reused_idempotency_key_with_different_intent_is_rejected() {
        let root = MailboxTestRoot::new();
        let path = root.database_path();
        let store = MailboxStore::open(&path).expect("mailbox store should open");
        store.append(&test_input()).expect("append should succeed");
        let mut conflicting = test_input();
        conflicting.body = "Different message".to_owned();
        let error = store
            .append(&conflicting)
            .expect_err("conflicting retry should fail");
        assert!(error.to_string().contains("different mailbox message"));
    }

    #[test]
    fn acknowledgement_is_forward_only_compare_and_swap() {
        let root = MailboxTestRoot::new();
        let path = root.database_path();
        let store = MailboxStore::open(&path).expect("mailbox store should open");
        let receipt = store.append(&test_input()).expect("append should succeed");

        let summary = store
            .acknowledge("session-target", &receipt.mailbox_id, 0, 1)
            .expect("matching cursor should advance");
        assert_eq!(summary.unread_count, 0);
        let error = store
            .acknowledge("session-target", &receipt.mailbox_id, 0, 1)
            .expect_err("stale cursor should conflict");
        assert!(error.to_string().contains("conflict"));
    }

    #[test]
    fn unread_count_includes_only_inbound_messages_above_the_cursor() {
        let root = MailboxTestRoot::new();
        let path = root.database_path();
        let store = MailboxStore::open(&path).expect("mailbox store should open");
        let first = store.append(&test_input()).expect("append should succeed");

        let mut reply = test_input();
        reply.sender_session_id = "session-target".to_owned();
        reply.sender_name = "Target".to_owned();
        reply.target_session_id = "session-sender".to_owned();
        reply.target_name = "Sender".to_owned();
        reply.idempotency_key = "reply-1".to_owned();
        reply.body = "Outbound from the original target".to_owned();
        store.append(&reply).expect("reply should append");

        let target_summary = store
            .list_for_session("session-target")
            .expect("target summary should read")
            .into_iter()
            .find(|summary| summary.id == first.mailbox_id)
            .expect("target mailbox should exist");
        assert_eq!(
            target_summary.unread_count, 1,
            "the target's own outbound reply must not inflate inbound unread"
        );
        assert_eq!(
            store
                .unread_wakeups_for_session("session-target")
                .expect("wake state should read")[0]
                .unread_count,
            1
        );
    }

    #[test]
    fn mailbox_append_caps_body_and_optional_metadata() {
        let mutations: [fn(&mut MailboxAppendInput); 3] = [
            |input: &mut MailboxAppendInput| {
                input.body = "x".repeat(MAX_MAILBOX_BODY_BYTES + 1);
            },
            |input: &mut MailboxAppendInput| {
                input.topic = Some("x".repeat(MAX_MAILBOX_METADATA_BYTES + 1));
            },
            |input: &mut MailboxAppendInput| {
                input.state_stamp = Some("x".repeat(MAX_MAILBOX_METADATA_BYTES + 1));
            },
        ];
        for mutate in mutations {
            let mut input = test_input();
            mutate(&mut input);
            assert!(
                validate_mailbox_append_input(&input)
                    .expect_err("oversized mailbox input should fail")
                    .to_string()
                    .contains("exceeds")
            );
        }
    }

    #[test]
    fn concurrent_appends_allocate_one_dense_mailbox_sequence() {
        let root = MailboxTestRoot::new();
        let path = root.database_path();
        let store = Arc::new(MailboxStore::open(&path).expect("mailbox store should open"));
        let barrier = Arc::new(std::sync::Barrier::new(5));
        let mut handles = Vec::new();
        for index in 0..4 {
            let store = store.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let mut input = test_input();
                input.idempotency_key = format!("send-{index}");
                input.body = format!("message {index}");
                barrier.wait();
                store.append(&input).expect("concurrent append should succeed")
            }));
        }
        barrier.wait();
        let mut receipts = handles
            .into_iter()
            .map(|handle| handle.join().expect("append thread should join"))
            .collect::<Vec<_>>();
        receipts.sort_by_key(|receipt| receipt.sequence);
        assert_eq!(
            receipts
                .iter()
                .map(|receipt| receipt.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(
            receipts
                .windows(2)
                .all(|pair| pair[0].mailbox_id == pair[1].mailbox_id)
        );
    }

    #[test]
    fn appending_again_does_not_resurrect_a_departed_participant() {
        let root = MailboxTestRoot::new();
        let path = root.database_path();
        let store = MailboxStore::open(&path).expect("mailbox store should open");
        let first = store.append(&test_input()).expect("append should succeed");
        store
            .mark_session_left("session-target")
            .expect("participant should be marked left");

        let mut second_input = test_input();
        second_input.idempotency_key = "send-2".to_owned();
        second_input.body = "second body".to_owned();
        let error = store
            .append(&second_input)
            .expect_err("append to a departed participant should be rejected");
        assert!(error.to_string().contains("departed mailbox participant"));

        assert!(
            store
                .list_for_session("session-target")
                .expect("departed participant list should read")
                .is_empty(),
            "append upsert must not clear a deletion's left marker"
        );
        let sender_summary = store
            .list_for_session("session-sender")
            .expect("sender mailbox list should read")
            .into_iter()
            .find(|summary| summary.id == first.mailbox_id)
            .expect("sender should retain mailbox history");
        assert!(sender_summary
            .participants
            .iter()
            .find(|participant| participant.session_id == "session-target")
            .expect("target snapshot should remain")
            .left_at
            .is_some());
    }
}
