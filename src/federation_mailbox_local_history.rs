// Selects original native mailbox ownership before domain/relay routing.
// Does not retry refused domain operations against a different store or actor.

impl AppState {
    async fn replay_original_native_mailbox_send(&self, native: &str, request: &SendMailboxMessageRequest)
        -> Result<Option<MailboxAppendReceipt>, ApiError>
    {
        let app = self.clone();
        let native = native.to_owned();
        let request = request.clone();
        run_blocking_api(move || {
            let creation = {
                let inner = app.inner.lock().expect("state mutex poisoned");
                federation_mailbox_root_locked(&inner, &native)?.federation_native_creation_id.clone()
            };
            let stored = {
                let Some(connection) = app.mailbox_store.connection_if_enabled() else { return Ok(None); };
                mailbox_message_for_idempotency_key(&connection, &native, &request.idempotency_key).map_err(mailbox_api_error)?
            };
            let Some(stored) = stored else { return Ok(None); };
            let target_matches = stored.target_session_id == request.target_session_id
                || app.canonical_local_native_mailbox_target(&request.target_session_id)?
                    .as_deref() == Some(stored.target_session_id.as_str());
            if request.class.as_deref().is_some_and(|class| class != "routine")
                || !target_matches || stored.body != request.message
                || stored.topic != request.topic || stored.state_stamp != request.state_stamp {
                return Err(ApiError::conflict("idempotency key belongs to a different original native mailbox message"));
            }
            let through = {
                let connection = app.mailbox_store.connection().map_err(mailbox_api_error)?;
                mailbox_processed_through(&connection, &stored.mailbox_id, &native).map_err(mailbox_api_error)?
            };
            let disposition = match stored.dispatch_outcome {
                Some(outcome) => outcome,
                None => app.mailbox_store.wait_for_final_dispatch_outcome(&stored.message_id).map_err(mailbox_api_error)?,
            };
            let inner = app.inner.lock().expect("state mutex poisoned");
            if federation_mailbox_root_locked(&inner, &native)?.federation_native_creation_id != creation {
                return Err(ApiError::conflict("native mailbox sender changed during retry"));
            }
            Ok(Some(MailboxAppendReceipt { notification_cause: None, mailbox_id: stored.mailbox_id,
                message_id: stored.message_id, sequence: stored.sequence, unread_depth: stored.unread_depth_at_append,
                notification_disposition: disposition, duplicate: true, sender_processed_through: through,
                sender_cursor_advanced: false, receipt_note: None }))
        }).await
    }

    async fn owns_original_mailbox_resource(&self, native: &str, id: &str, message: bool) -> Result<bool, ApiError> {
        let store = self.mailbox_store.clone();
        let native = native.to_owned();
        let id = id.to_owned();
        run_blocking_api(move || {
            let Some(connection) = store.connection_if_enabled() else { return Ok(false); };
            let sql = if message {
                "SELECT EXISTS(SELECT 1 FROM mailbox_participants p JOIN mailbox_messages m ON m.mailbox_id=p.mailbox_id WHERE p.session_id=?1 AND m.id=?2)"
            } else {
                "SELECT EXISTS(SELECT 1 FROM mailbox_participants WHERE session_id=?1 AND mailbox_id=?2)"
            };
            connection.query_row(sql, rusqlite::params![native, id], |row| row.get::<_, bool>(0)).map_err(|error| mailbox_api_error(anyhow!(error)))
        }).await
    }
}
