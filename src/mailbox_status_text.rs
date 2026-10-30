// The one plain-language wording of a mailbox message's wake and
// acknowledgement state. The HTTP API returns these strings, and the
// delegation tools, the CLI and the UI show them as returned, so every
// reader sees the same sentence for the same state.
// Owns: the send-receipt line (the wake status AT SEND TIME and the next
// action), the per-state wake sentence, and the exact read's "as of" summary.
// Does not own: reading or storing any state (src/mailboxes.rs). It promises
// no wake, retry or delivery, and never asks for a resend or a poll.
// New module beside src/mailboxes.rs, not split out of it.

/// The optional next action every receipt line names, with the message's
/// own returned id: a local id or a relay locator, never a rebuilt one.
fn mailbox_current_status_action(message_id: &str) -> String {
    format!(
        "If current status is needed, use termal_read_mailbox_message({{messageId: \"{message_id}\"}}) or `termal mailbox read-message --message-id {message_id}`."
    )
}

/// One sentence for a wake state. It says nothing about acknowledgement, and
/// the states are not a ladder: no wording claims a wake never happened.
fn mailbox_wake_state_sentence(notification_state: &str) -> String {
    match notification_state {
        "deliveredToIdleSession" => "Notification delivered to the recipient's session.".to_owned(),
        "queuedBehindActiveTurn" => {
            "Wake queued; it waits for the recipient's active turn to end.".to_owned()
        }
        "recoveredWake" => "Wake queued for the recipient by recovery.".to_owned(),
        "heldBehindPausedQueue" => {
            "Wake held; it waits for the recipient's paused queue to be resumed.".to_owned()
        }
        "waitingForAutomaticRetry" => {
            "Wake held; it waits for the recipient's due automatic retry, which clears it by itself."
                .to_owned()
        }
        "waitingForProviderCapacity" => {
            "Wake held; it waits for provider capacity.".to_owned()
        }
        "durableButNotWoken" => "No wake confirmed; the message remains stored.".to_owned(),
        other => format!(
            "Wake state `{other}` is not one this host recognises; the message remains stored."
        ),
    }
}

/// The send receipt's line. Its disposition is the wake status AT SEND TIME
/// and never changes; it is not current status.
fn mailbox_send_receipt_note(notification_disposition: &str, message_id: &str, duplicate: bool) -> String {
    let lead = if duplicate {
        "Duplicate send: the original message was saved earlier."
    } else {
        "Message saved."
    };
    // A duplicate always names the disposition its receipt carries AT SEND TIME.
    let at_send = if notification_disposition == "durableButNotWoken" && !duplicate {
        "Wake was unconfirmed when sent; this is not current status.".to_owned()
    } else {
        format!(
            "Wake status AT SEND TIME: {notification_disposition} ({}); this is not current status.",
            mailbox_wake_state_sentence(notification_disposition).trim_end_matches('.')
        )
    };
    format!(
        "{lead} {at_send} Do not resend based on this receipt alone. {}",
        mailbox_current_status_action(message_id)
    )
}

/// The exact read's summary: the wake state as of the read, then the
/// recipient's acknowledgement on its own. An acknowledgement is the
/// recipient's protocol acknowledgement, not proof the content was acted on,
/// and a message can be acknowledged without any wake.
fn mailbox_current_status_summary(
    observed_at: &str,
    notification_state: &str,
    recipient_acknowledged: bool,
) -> String {
    let acknowledgement = if recipient_acknowledged {
        "The recipient has acknowledged it."
    } else {
        "Not yet acknowledged by the recipient."
    };
    format!(
        "As of {observed_at}: {} {acknowledgement}",
        mailbox_wake_state_sentence(notification_state)
    )
}

/// An exact read whose remote mailbox home could not be reached or gave no
/// usable answer. Only the cause's first clause is kept: guidance written for
/// another operation, such as retrying an original send, does not apply here.
fn mailbox_current_status_unavailable_text(cause: &str) -> String {
    let cause = cause.split(';').next().unwrap_or(cause).trim();
    format!(
        "current status unavailable: the message's mailbox home could not be reached or gave no usable answer, which says nothing about delivery; try the read again later ({cause})"
    )
}
