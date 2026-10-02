/*
Acknowledged mailbox wakes: a wake accepted behind an active turn survives the
receiver acknowledging the same mail in that turn.

Owns:
- the continuation text a kept wake carries once its mail is acknowledged;
- the acknowledgement-time refresh of covered queued wakes (both the receipt
  and the legacy numeric acknowledgement routes);
- the store read of one wake's boundary row, which dispatch-time revalidation
  in `mailboxes.rs` uses to tell an owed continuation from a wake a handoff
  already covered.

Does not own:
- the decision to drop or keep a queue head at dispatch time, which stays in
  `revalidate_front_mailbox_wakeup_for_session` (`mailboxes.rs`);
- delivery, failure recovery, restart reconciliation or Stop handling.

New beside `mailboxes.rs`; it replaces the acknowledgement-time removal of
covered wakes that lived there.
*/

/// The boundary row of a queued mailbox wake, read off the state lock.
#[derive(Clone, Debug, PartialEq, Eq)]
struct MailboxWakeBoundary {
    sender_session_id: String,
    sender_name: String,
    topic: Option<String>,
    /// The row's mutable delivery record says a handoff covered it.
    delivered: bool,
}

impl MailboxStore {
    /// Reads the boundary row of a queued wake. `None` means the row is not
    /// visible to the session: it is gone, belongs to another target or
    /// mailbox, is a delegation review result, the session has left the
    /// mailbox, or the store is disabled.
    fn mailbox_wake_boundary(
        &self,
        session_id: &str,
        mailbox_id: &str,
        message_id: &str,
    ) -> Result<Option<MailboxWakeBoundary>> {
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(None);
        };
        let result = connection.query_row(
            "SELECT message.sender_session_id, message.sender_name, message.topic,
                    message.notification_disposition
             FROM mailbox_messages message
             JOIN mailbox_participants mine
               ON mine.mailbox_id = message.mailbox_id
              AND mine.session_id = ?1
              AND mine.left_at IS NULL
             WHERE message.id = ?3
               AND message.mailbox_id = ?2
               AND message.target_session_id = ?1
               AND COALESCE(message.topic, '') != ?4",
            rusqlite::params![
                session_id,
                mailbox_id,
                message_id,
                DELEGATION_REVIEW_RESULT_TOPIC
            ],
            |row| {
                Ok(MailboxWakeBoundary {
                    sender_session_id: row.get(0)?,
                    sender_name: row.get(1)?,
                    topic: row.get(2)?,
                    delivered: row.get::<_, String>(3)? == "deliveredToIdleSession",
                })
            },
        );
        match result {
            Ok(boundary) => Ok(Some(boundary)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(anyhow!(err).context("failed to read mailbox wake boundary")),
        }
    }
}

/// Text of a kept wake whose mail the receiver already acknowledged. It never
/// claims unread mail, and it tells the agent not to answer the wake itself.
fn mailbox_continuation_notification_text(
    mailbox_id: &str,
    sequence: u64,
    sender_name: &str,
    topic: Option<&str>,
) -> String {
    let sender_name = mailbox_preview(sender_name);
    let topic_line = topic.map_or(String::new(), |topic| {
        format!("Topic: {}\n", mailbox_preview(topic))
    });
    format!(
        "[TermAl mailbox notification]\n\
         Mailbox `{mailbox_id}`: messages through #{sequence} (latest from {sender_name}) were accepted while this wake was queued and are already acknowledged. Nothing is unread.\n\
         {topic_line}This is the queued continuation wake: resume the next-turn work your context calls for, or end the turn if there is none. Do not send a reply only to acknowledge it."
    )
}

/// Rewrites a queued wake in place as the continuation for its boundary.
/// Identity, position and boundary stay unchanged.
fn rewrite_queued_wake_as_continuation(
    queued: &mut QueuedPromptRecord,
    mailbox_id: &str,
    message_id: &str,
    sequence: u64,
    boundary: MailboxWakeBoundary,
) -> bool {
    let text = mailbox_continuation_notification_text(
        mailbox_id,
        sequence,
        &boundary.sender_name,
        boundary.topic.as_deref(),
    );
    let source = MessageSource::mailbox(
        boundary.sender_session_id,
        boundary.sender_name,
        MailboxMessageSource {
            mailbox_id: mailbox_id.to_owned(),
            message_id: message_id.to_owned(),
            sequence,
            unread_count: 0,
        },
    );
    if queued.pending_prompt.text == text && queued.pending_prompt.source.as_ref() == Some(&source)
    {
        return false;
    }
    queued.pending_prompt.timestamp = stamp_now();
    queued.pending_prompt.text = text;
    queued.pending_prompt.source = Some(source);
    true
}

impl AppState {
    /// Keeps every queued wake an acknowledgement covers and rewrites its
    /// text so it no longer claims unread mail.
    ///
    /// Acknowledgement advances the data cursor only; it never cancels an
    /// accepted wake. Retained heads are replay input and stay untouched. A
    /// wake whose boundary row is invisible or already delivered is left for
    /// dispatch-time revalidation, which drops it.
    fn refresh_acknowledged_mailbox_wakeups(
        &self,
        session_id: &str,
        mailbox_id: &str,
        processed_through: u64,
    ) -> Result<bool> {
        let covered: Vec<(String, String, u64)> = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return Ok(false);
            };
            inner.sessions[index]
                .queued_prompts
                .iter()
                .filter(|queued| !queued.is_engram_retained())
                .filter_map(|queued| {
                    let mailbox = queued.pending_prompt.source.as_ref()?.mailbox.as_ref()?;
                    (mailbox.mailbox_id == mailbox_id && mailbox.sequence <= processed_through)
                        .then(|| {
                            (
                                queued.pending_prompt.id.clone(),
                                mailbox.message_id.clone(),
                                mailbox.sequence,
                            )
                        })
                })
                .collect()
        };
        let mut changed = false;
        for (prompt_id, message_id, sequence) in covered {
            // Never hold the mailbox connection together with StateInner; the
            // prompt id and boundary are rechecked under the state lock.
            let Some(boundary) =
                self.mailbox_store
                    .mailbox_wake_boundary(session_id, mailbox_id, &message_id)?
            else {
                continue;
            };
            if boundary.delivered {
                continue;
            }
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return Ok(changed);
            };
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            let Some(queued) = record.queued_prompts.iter_mut().find(|queued| {
                queued.pending_prompt.id == prompt_id
                    && !queued.is_engram_retained()
                    && queued
                        .pending_prompt
                        .source
                        .as_ref()
                        .and_then(|source| source.mailbox.as_ref())
                        .is_some_and(|mailbox| {
                            mailbox.mailbox_id == mailbox_id
                                && mailbox.message_id == message_id
                                && mailbox.sequence == sequence
                        })
            }) else {
                continue;
            };
            if rewrite_queued_wake_as_continuation(
                queued,
                mailbox_id,
                &message_id,
                sequence,
                boundary,
            ) {
                sync_pending_prompts(record);
                self.commit_locked(&mut inner)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    fn acknowledge_mailbox_and_refresh_covered_wakeups(
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
            self.refresh_acknowledged_mailbox_wakeups(session_id, mailbox_id, processed_through)
        {
            // The durable CAS already committed. Returning an error would make
            // a correct retry conflict on the old expected cursor; dispatch-
            // time revalidation rewrites the kept wake instead.
            eprintln!(
                "mailbox> acknowledgement committed but queued-wake refresh failed for \
                 `{session_id}` / `{mailbox_id}`: {err:#}"
            );
        }
        Ok(summary)
    }
}
