// Owns central directory-key mailbox creation and authorized read adapters.
// Does not choose an authority or maintain a satellite replica. New adapters
// reuse mailboxes.rs transactions, deduplication, issuance, and cursor CAS.

impl MailboxStore {
    fn domain_get_or_create(
        &self,
        actor: &FederationMailboxParticipant,
        target: &FederationMailboxParticipant,
        authority: &FederationMailboxAuthority,
    ) -> Result<MailboxSummary> {
        let _writer = self.lock_writer_with_budget("creating central mailbox membership", Duration::from_secs(2))?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "creating central mailbox membership")?;
        let key = mailbox_participant_key(&actor.session_id, &target.session_id);
        let existing: Option<String> = transaction
            .query_row(
                "SELECT id FROM mailboxes WHERE participant_key=?1",
                [&key],
                |row| row.get(0),
            )
            .optional()?;
        let id = existing.unwrap_or_else(|| format!("mailbox-{}", Uuid::new_v4()));
        let now = stamp_now();
        transaction.execute("INSERT INTO mailboxes(id,participant_key,created_at,next_sequence) VALUES(?1,?2,?3,1) ON CONFLICT(participant_key) DO NOTHING",
            rusqlite::params![id,key,now])?;
        upsert_mailbox_participant(&transaction, &id, &actor.session_id, &actor.name, &now, false)?;
        upsert_mailbox_participant(&transaction, &id, &target.session_id, &target.name, &now, false)?;
        let summary =
            mailbox_summaries_for_session_with_empty(&transaction, &actor.session_id, true)?
                .into_iter()
                .find(|row| row.id == id)
                .ok_or_else(|| anyhow!("central mailbox membership disappeared"))?;
        let _authority = authority.lock()?;
        transaction.commit()?;
        Ok(summary)
    }

    fn domain_list(
        &self,
        actor: &str,
        authority: &FederationMailboxAuthority,
    ) -> Result<Vec<MailboxSummary>> {
        let connection = self.connection()?;
        let summaries = mailbox_summaries_for_session_with_empty(&connection, actor, true)?;
        let _authority = authority.lock()?;
        Ok(summaries)
    }

    fn domain_read_message(
        &self,
        actor: &str,
        message_id: &str,
        authority: &FederationMailboxAuthority,
    ) -> Result<MailboxMessage> {
        // Take the store resource before link/state, exactly as the write
        // adapters do; never call a locking store helper under StateInner.
        let connection = self.connection()?;
        // One read transaction, as for a local exact read: the message, the
        // membership and the recipient's cursor come from one snapshot.
        let read = connection
            .unchecked_transaction()
            .context("failed to begin the mailbox message read")?;
        let mut message = read
            .query_row(
                "SELECT id, mailbox_id, sequence, sender_session_id, sender_name,
             target_session_id,target_name,created_at,class,topic,state_stamp,body,
             idempotency_key,unread_depth_at_append,notification_disposition
             FROM mailbox_messages WHERE id=?1
             AND NOT(target_session_id=?2 AND COALESCE(topic,'') GLOB ?3)",
                rusqlite::params![message_id, actor, DELEGATION_REVIEW_CONTROL_TOPIC_PATTERN],
                mailbox_message_from_row,
            )
            .optional()?
            .ok_or_else(|| {
                mailbox_store_error(MailboxStoreErrorKind::NotFound, "mailbox message not found")
            })?;
        require_mailbox_participant(&read, &message.mailbox_id, actor)?;
        attach_mailbox_message_current_status(&read, &mut message)?;
        let _authority = authority.lock()?;
        Ok(message)
    }
}
