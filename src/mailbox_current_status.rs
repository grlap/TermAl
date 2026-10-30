// The exact-message read's current-status projection: what one read observed
// about a message (its wake state and its recipient's acknowledgement), and how
// a remote home that cannot answer is reported.
// Owns: MailboxMessageCurrentStatus, attach_mailbox_message_current_status and
// mailbox_current_status_unavailable.
// Does not own: the wording (src/mailbox_status_text.rs), the store reads that
// call it, or the routes (src/mailboxes.rs, src/mailbox_relay_routes.rs).
// New module beside src/mailboxes.rs, written with this change.

/// What an exact read observed about one message at `observed_at`: its
/// current wake state, whether its recipient has acknowledged it (the
/// recipient's own cursor reached it; the sender's never counts), and the
/// shared summary (`mailbox_current_status_summary`). Derived at read time;
/// nothing is stored.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct MailboxMessageCurrentStatus {
    observed_at: String,
    notification_state: String,
    recipient_acknowledged: bool,
    summary: String,
}

/// Adds the current projection to a message just read on `connection`, from
/// the same read: the message's own wake state and its recipient's cursor.
/// Reading issues no page receipt and moves no cursor.
fn attach_mailbox_message_current_status(
    connection: &rusqlite::Connection,
    message: &mut MailboxMessage,
) -> Result<()> {
    let recipient_processed_through = connection
        .query_row(
            "SELECT processed_through
             FROM mailbox_participants
             WHERE mailbox_id = ?1 AND session_id = ?2",
            rusqlite::params![&message.mailbox_id, &message.target_session_id],
            |row| row.get::<_, u64>(0),
        )
        .optional()
        .context("failed to read the recipient's mailbox cursor")?;
    let recipient_acknowledged =
        recipient_processed_through.is_some_and(|processed| processed >= message.sequence);
    let observed_at = chrono::Utc::now().to_rfc3339();
    message.current_status = Some(MailboxMessageCurrentStatus {
        summary: mailbox_current_status_summary(
            &observed_at,
            &message.notification_state,
            recipient_acknowledged,
        ),
        observed_at,
        notification_state: message.notification_state.clone(),
        recipient_acknowledged,
    });
    Ok(())
}

/// An exact read whose remote mailbox home could not answer says that the
/// message's current status is unavailable. It keeps the error's status and
/// cause, and never substitutes the send receipt or a cached state, nor
/// reads the outage as a failed delivery.
fn mailbox_current_status_unavailable(mut error: ApiError) -> ApiError {
    if matches!(
        error.status,
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
    ) {
        error.message = mailbox_current_status_unavailable_text(&error.message);
    }
    error
}
