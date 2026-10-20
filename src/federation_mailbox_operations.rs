// Owns central execution of typed mailbox operations and truthful outcomes.
// Does not relay requests or drain the durable remote wake obligations. New
// application layer over the qualified directory and existing mailbox store.

impl AppState {
    fn execute_federation_mailbox(
        &self,
        request: FederationMailboxRequest,
        lease: Option<FederationLinkLease>,
    ) -> Result<FederationMailboxResponse, ApiError> {
        if request.protocol_version != FEDERATION_MAILBOX_WIRE_VERSION
            || request.schema_version != FEDERATION_MAILBOX_SCHEMA_VERSION
        {
            return Err(ApiError::bad_request(
                "unsupported central mailbox protocol or schema; negotiate before retrying",
            ));
        }
        let central =
            federation_mailbox_central_locked(&self.inner.lock().expect("state mutex poisoned"))?;
        if request.central != central {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        if matches!(request.operation, FederationMailboxOperation::Protocol) {
            return federation_mailbox_response(
                central.clone(),
                &federation_mailbox_protocol(central),
            );
        }
        let actor = request
            .actor
            .ok_or_else(|| ApiError::bad_request("qualified mailbox actor required"))?;
        let participant = if let Some(lease) = &lease {
            let peer = lease.with_current(self, |_, peer| Ok(peer.clone()))?;
            self.register_federation_mailbox_actor(&peer, &actor)?
        } else {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .federation
                .mailbox_directory
                .get(&actor.binding.participant_key())
                .filter(|row| {
                    row.binding == actor.binding
                        && row.binding.identity.host_id == central.host_id
                        && federation_mailbox_participant_current_locked(&inner, row)
                })
                .and_then(|row| current_federation_mailbox_participant_locked(&inner, row))
                .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?
        };
        let mut authority = FederationMailboxAuthority {
            app: self.clone(),
            actor: participant.binding.clone(),
            lease,
            target: None,
        };
        match request.operation {
            FederationMailboxOperation::Protocol => Err(ApiError::bad_request(
                "protocol operation does not require an actor",
            )),
            FederationMailboxOperation::Register => {
                federation_mailbox_response(central, &participant)
            }
            FederationMailboxOperation::Directory => {
                let guard = authority.lock().map_err(mailbox_api_error)?;
                let entries: Vec<_> = guard
                    ._inner
                    .federation
                    .mailbox_directory
                    .values()
                    .filter_map(|row| {
                        current_federation_mailbox_participant_locked(&guard._inner, row)
                    })
                    .collect();
                federation_mailbox_response(central, &entries)
            }
            FederationMailboxOperation::GetOrCreate { target } => {
                let target = resolve_federation_mailbox_participant_locked(
                    &self.inner.lock().expect("state mutex poisoned"),
                    &target,
                )?;
                if participant.session_id == target.session_id {
                    return Err(ApiError::bad_request(
                        "mailbox target must be another participant",
                    ));
                }
                authority.target = Some(target.clone());
                let mailbox = self
                    .mailbox_store
                    .domain_get_or_create(&participant, &target, &authority)
                    .map_err(mailbox_api_error)?;
                federation_mailbox_response(central, &mailbox)
            }
            FederationMailboxOperation::Send {
                target,
                message,
                key,
                topic,
                state_stamp,
            } => {
                let target = resolve_federation_mailbox_participant_locked(
                    &self.inner.lock().expect("state mutex poisoned"),
                    &target,
                )?;
                if participant.session_id == target.session_id {
                    return Err(ApiError::bad_request(
                        "mailbox target must be another participant",
                    ));
                }
                let delivery = FederationDelivery {
                    version: 1,
                    sender: participant.binding.clone(),
                    target: target.binding.clone(),
                    key,
                    message,
                    topic,
                    state_stamp,
                };
                self.execute_federation_delivery(central, participant, target, delivery, authority)
            }
            FederationMailboxOperation::SendExact { delivery } => {
                self.execute_exact_federation_delivery(central, participant, delivery, authority)
            }
            FederationMailboxOperation::CancelExact { delivery } => {
                delivery.validate()?;
                if delivery.sender != participant.binding {
                    return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                }
                self.mailbox_store.cancel_federation_delivery(&delivery, || authority.lock())
                    .map_err(mailbox_api_error)?;
                federation_mailbox_response(central, &FederationDeliveryTerminal {
                    state: FederationDeliveryTerminalState::Abandoned,
                    canonical_digest: delivery.digest(),
                })
            }
            FederationMailboxOperation::List => {
                let rows = self
                    .mailbox_store
                    .domain_list(&participant.session_id, &authority)
                    .map_err(mailbox_api_error)?;
                federation_mailbox_response(central, &rows)
            }
            FederationMailboxOperation::Read {
                mailbox_id,
                after_sequence,
                limit,
                issue_receipt,
            } => {
                let mut page = self
                    .mailbox_store
                    .read_range_authorized(
                        &participant.session_id,
                        &mailbox_id,
                        after_sequence,
                        limit.clamp(1, FEDERATION_MAILBOX_PAGE_LIMIT),
                        issue_receipt,
                        || authority.lock(),
                    )
                    .map_err(mailbox_api_error)?;
                for message in &mut page.messages {
                    self.federation_mailbox_message_cause(message);
                }
                page.reader_session_id = Some(participant.session_id);
                federation_mailbox_response(central, &page)
            }
            FederationMailboxOperation::ReadMessage { message_id } => {
                let mut message = self
                    .mailbox_store
                    .domain_read_message(&participant.session_id, &message_id, &authority)
                    .map_err(mailbox_api_error)?;
                self.federation_mailbox_message_cause(&mut message);
                federation_mailbox_response(central, &message)
            }
            FederationMailboxOperation::Acknowledge {
                mailbox_id,
                receipt,
            } => {
                let summary = self
                    .mailbox_store
                    .acknowledge_authorized(
                        &participant.session_id,
                        &mailbox_id,
                        Some(&receipt),
                        0,
                        0,
                        || authority.lock(),
                    )
                    .map_err(mailbox_api_error)?;
                if participant.binding.identity.host_id == central.host_id {
                    let through = summary
                        .participants
                        .iter()
                        .find(|row| row.session_id == participant.session_id)
                        .map_or(0, |row| row.processed_through);
                    if let Err(error) = self.refresh_acknowledged_mailbox_wakeups(
                        &participant.binding.identity.session_id,
                        &mailbox_id,
                        through,
                    ) {
                        eprintln!(
                            "central mailbox acknowledgement committed; local wake refresh failed: {error:#}"
                        );
                    }
                }
                federation_mailbox_response(central, &summary)
            }
        }
    }

    fn execute_federation_delivery(
        &self,
        central: FederationHostIdentity,
        participant: FederationMailboxParticipant,
        target: FederationMailboxParticipant,
        delivery: FederationDelivery,
        mut authority: FederationMailboxAuthority,
    ) -> Result<FederationMailboxResponse, ApiError> {
        delivery.validate()?;
        authority.target = Some(target.clone());
        let input = delivery.input(participant.name.clone(), target.name.clone());
        let appended = self.mailbox_store
            .append_with_delivery(&input, Some(&delivery), || authority.lock())
            .map_err(mailbox_api_error)?;
        if target.binding.identity.host_id == central.host_id {
            let receipt = self.finish_mailbox_append_and_notify(
                &participant.session_id, &target.binding.identity.session_id, &input, appended)?;
            return federation_mailbox_response(central, &receipt);
        }
        let MailboxAppendResult { mut receipt, finalization } = appended;
        if !receipt.duplicate {
            if let Err(error) = self.mailbox_store
                .record_initial_dispatch_outcome(&receipt.message_id, "durableButNotWoken")
            {
                eprintln!("central mailbox message {} committed; dispatch outcome recording failed: {error:#}", receipt.message_id);
            }
        }
        drop(finalization);
        self.federation_executor_wake_signal.kick();
        receipt.notification_cause = Some(FEDERATION_REMOTE_WAKE_CAUSE.to_owned());
        federation_mailbox_response(central, &receipt)
    }

    fn federation_mailbox_message_cause(&self, message: &mut MailboxMessage) {
        let inner = self.inner.lock().expect("state mutex poisoned");
        if inner
            .federation
            .mailbox_directory
            .get(&message.target_session_id)
            .is_some_and(|target| target.binding.identity.host_id != target.binding.central.host_id)
        {
            drop(inner);
            if self.refresh_federation_executor_reservation_domain().is_err() {
                message.notification_cause = Some("remote wake state unavailable; authority not durable".into());
                return;
            }
            message.notification_cause = Some(self.mailbox_store.federation_executor_wake_state(&message.id)
                .unwrap_or_else(|_| "remote wake state unavailable; ownership not inferred".to_owned()));
        }
    }
}

fn federation_mailbox_response(
    central: FederationHostIdentity,
    value: &impl Serialize,
) -> Result<FederationMailboxResponse, ApiError> {
    Ok(FederationMailboxResponse {
        central,
        protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
        schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
        value: serde_json::to_value(value)
            .map_err(|_| ApiError::internal("central mailbox response encoding failed"))?,
    })
}
