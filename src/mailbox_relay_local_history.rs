// A native root's already committed home-local history is its own data even
// after the remote peer disconnects or is revoked. This grants no wake/send.
struct MailboxRelayLocalHistoryAuthority {
    app: AppState,
    actor: MailboxRelayPrincipal,
    home: MailboxRelayHome,
}

impl MailboxRelayLocalHistoryAuthority {
    fn lock(&self) -> Result<FederationMailboxAdmission<'_>> {
        let inner = self.app.inner.lock().expect("state mutex poisoned");
        let root = federation_mailbox_root_locked(&inner, &self.actor.native_session_id)
            .map_err(|error| relay_store_error(&error.message))?;
        if self.actor.host != self.home.host
            || self.actor.store_id != self.home.store_id
            || inner.federation.local.as_ref() != Some(&self.actor.host)
            || root.federation_native_creation_id != self.actor.native_creation_id
        {
            return Err(relay_store_error(
                "native mailbox local history owner changed",
            ));
        }
        Ok(FederationMailboxAdmission {
            _inner: inner,
            _link: None,
        })
    }
}

impl AppState {
    fn execute_local_mailbox_relay_history(
        &self,
        actor: &MailboxRelayPrincipal,
        home: &MailboxRelayHome,
        operation: MailboxRelayOperation,
    ) -> Result<Value, ApiError> {
        actor.validate()?;
        home.validate()?;
        let store_id = {
            let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
            mailbox_relay_store_id(&connection).map_err(mailbox_api_error)?
        };
        if actor.store_id != store_id || home.store_id != store_id || actor.host != home.host {
            return Err(ApiError::conflict(
                "native mailbox local home/store changed",
            ));
        }
        let authority = MailboxRelayLocalHistoryAuthority {
            app: self.clone(),
            actor: actor.clone(),
            home: home.clone(),
        };
        let value = match operation {
            MailboxRelayOperation::List {
                actor: request_actor,
                home: request_home,
            } => {
                if request_actor != *actor || request_home != *home {
                    return Err(ApiError::conflict(
                        "native mailbox local list owner changed",
                    ));
                }
                let rows = {
                    let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
                    mailbox_summaries_for_session_with_empty(
                        &connection,
                        &actor.participant_key(),
                        true,
                    )
                    .map_err(mailbox_api_error)?
                };
                let rows = rows
                    .into_iter()
                    .map(|summary| {
                        let conversation = self
                            .mailbox_store
                            .mailbox_relay_association(actor, &summary.id)
                            .map_err(mailbox_api_error)?
                            .ok_or_else(|| {
                                ApiError::conflict("native mailbox home association missing")
                            })?;
                        conversation.require_home(home)?;
                        Ok(MailboxRelaySummary {
                            conversation,
                            summary,
                        })
                    })
                    .collect::<Result<Vec<_>, ApiError>>()?;
                let _guard = authority.lock().map_err(mailbox_api_error)?;
                serde_json::to_value(rows)
            }
            MailboxRelayOperation::Read {
                actor: request_actor,
                conversation,
                mailbox_id,
                after_sequence,
                limit,
                issue_receipt,
            } => {
                if request_actor != *actor {
                    return Err(ApiError::conflict("native mailbox local reader changed"));
                }
                conversation.require_home(home)?;
                self.require_mailbox_relay_conversation(actor, &mailbox_id, &conversation)?;
                let mut page = self
                    .mailbox_store
                    .read_range_authorized(
                        &actor.participant_key(),
                        &mailbox_id,
                        after_sequence,
                        limit,
                        issue_receipt,
                        || authority.lock(),
                    )
                    .map_err(mailbox_api_error)?;
                page.reader_session_id = Some(actor.participant_key());
                serde_json::to_value(page)
            }
            MailboxRelayOperation::ReadMessage {
                actor: request_actor,
                home: request_home,
                message_id,
            } => {
                if request_actor != *actor || request_home != *home {
                    return Err(ApiError::conflict(
                        "native mailbox local message owner changed",
                    ));
                }
                let message = self
                    .mailbox_store
                    .read_message(&actor.participant_key(), &message_id)
                    .map_err(mailbox_api_error)?;
                let conversation = self
                    .mailbox_store
                    .mailbox_relay_association(actor, &message.mailbox_id)
                    .map_err(mailbox_api_error)?
                    .ok_or_else(|| ApiError::not_found("native mailbox association not found"))?;
                conversation.require_home(home)?;
                let _guard = authority.lock().map_err(mailbox_api_error)?;
                serde_json::to_value(message)
            }
            MailboxRelayOperation::Acknowledge {
                actor: request_actor,
                conversation,
                mailbox_id,
                receipt,
            } => {
                if request_actor != *actor {
                    return Err(ApiError::conflict(
                        "native mailbox local acknowledgement owner changed",
                    ));
                }
                conversation.require_home(home)?;
                self.require_mailbox_relay_conversation(actor, &mailbox_id, &conversation)?;
                let summary = self
                    .mailbox_store
                    .acknowledge_authorized(
                        &actor.participant_key(),
                        &mailbox_id,
                        Some(&receipt),
                        0,
                        0,
                        || authority.lock(),
                    )
                    .map_err(mailbox_api_error)?;
                serde_json::to_value(summary)
            }
            _ => {
                return Err(ApiError::bad_request(
                    "local history authority cannot send or dispatch",
                ));
            }
        };
        value.map_err(|_| ApiError::internal("native mailbox local history encoding failed"))
    }
}
