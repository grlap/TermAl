// Owns post-append local wake dispatch and its durable disposition.
// Does not append messages, authorize domain callers or deliver remote wakes.
// Split from mailboxes.rs so central routing reuses the existing local behavior.

impl AppState {
    fn finish_mailbox_append_and_notify(
        &self,
        sender_session_id: &str,
        target_runtime_session_id: &str,
        input: &MailboxAppendInput,
        appended: MailboxAppendResult,
    ) -> Result<MailboxAppendReceipt, ApiError> {
        let sender_name = &input.sender_name;
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
        let (_, target_still_active) =
            self.mailbox_participants_still_active(sender_session_id, target_runtime_session_id);
        if !target_still_active {
            if let Err(err) = self
                .mailbox_store
                .record_initial_dispatch_outcome(&receipt.message_id, "durableButNotWoken")
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
            input.topic.as_deref(),
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

        let disposition = match self.dispatch_turn(target_runtime_session_id, notification_request)
        {
            Ok(DispatchTurnResult::Dispatched(dispatch)) => {
                match deliver_turn_dispatch(self, dispatch) {
                    TurnDispatchDeliveryOutcome::Delivered
                    | TurnDispatchDeliveryOutcome::Scheduled => Some("deliveredToIdleSession"),
                    TurnDispatchDeliveryOutcome::Rejected(err) => {
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
                    TurnDispatchDeliveryOutcome::Held { error } => {
                        if let Some(error) = error {
                            eprintln!(
                                "mailbox> held wake for target session `{}` mailbox `{}` message `{}` after persistence uncertainty ({}): {}",
                                input.target_session_id,
                                receipt.mailbox_id,
                                receipt.message_id,
                                error.status,
                                error.message
                            );
                        }
                        None
                    }
                    TurnDispatchDeliveryOutcome::Superseded => None,
                }
            }
            Ok(DispatchTurnResult::DispatchedAfterQueue(dispatch)) => {
                match deliver_turn_dispatch(self, dispatch) {
                    TurnDispatchDeliveryOutcome::Delivered
                    | TurnDispatchDeliveryOutcome::Scheduled => Some("queuedBehindActiveTurn"),
                    TurnDispatchDeliveryOutcome::Rejected(err) => {
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
                    TurnDispatchDeliveryOutcome::Held { error } => {
                        if let Some(error) = error {
                            eprintln!(
                                "mailbox> held queued wake for target session `{}` mailbox `{}` message `{}` after persistence uncertainty ({}): {}",
                                input.target_session_id,
                                receipt.mailbox_id,
                                receipt.message_id,
                                error.status,
                                error.message
                            );
                        }
                        None
                    }
                    TurnDispatchDeliveryOutcome::Superseded => None,
                }
            }
            Ok(DispatchTurnResult::Queued) => {
                Some(self.mailbox_queued_wake_disposition(target_runtime_session_id))
            }
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
            Ok(MailboxDispatchOutcomeRecord::AlreadyFinalized { dispatch_outcome }) => {
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
}
