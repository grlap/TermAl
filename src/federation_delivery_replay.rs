// Owns exact retained-outcome replay and definitive target-retirement fencing.
// Does not authorize a new append from an old receipt or interpret lost links
// as retirement. SQLite admission precedes the commit-adjacent state guard.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum FederationDeliveryTerminalState {
    Refused,
    Abandoned,
}

impl FederationDeliveryTerminalState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Refused => "refused",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationDeliveryTerminal {
    state: FederationDeliveryTerminalState,
    canonical_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum FederationDeliveryOutcome {
    Committed(MailboxAppendReceipt),
    Terminal(FederationDeliveryTerminal),
}

impl AppState {
    fn execute_exact_federation_delivery(
        &self,
        central: FederationHostIdentity,
        participant: FederationMailboxParticipant,
        delivery: FederationDelivery,
        authority: FederationMailboxAuthority,
    ) -> Result<FederationMailboxResponse, ApiError> {
        delivery.validate()?;
        if delivery.sender != participant.binding {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        // Replay is authorized only by the exact current sender. A completed
        // target's authority must not be required to read an immutable outcome.
        let sender_authority = FederationMailboxAuthority {
            app: self.clone(), actor: participant.binding.clone(),
            lease: authority.lease.clone(), target: None,
        };
        if let Some(outcome) = self.mailbox_store.retained_federation_delivery(
            &delivery, || sender_authority.lock(),
        ).map_err(mailbox_api_error)? {
            return federation_mailbox_response(central, &outcome);
        }
        let attempted = (|| {
            let target = resolve_federation_mailbox_participant_locked(
                &self.inner.lock().expect("state mutex poisoned"),
                &delivery.target.participant_key(),
            );
            match target {
                Ok(target) => {
                    if target.binding != delivery.target {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    self.execute_federation_delivery(
                        central.clone(), participant, target, delivery.clone(), authority,
                    )
                }
                Err(error) if error.status == StatusCode::NOT_FOUND => {
                    self.mailbox_store.refuse_federation_delivery(&delivery, || {
                        let guard = sender_authority.lock()?;
                        if !federation_delivery_target_retired_locked(&guard._inner, &delivery.target) {
                            return Err(mailbox_store_error(MailboxStoreErrorKind::Retryable,
                                "captured recipient authority is temporarily unavailable; delivery outcome remains unknown; retry the exact delivery"));
                        }
                        // The definitive retirement witness remains locked
                        // through the refusal transaction's COMMIT.
                        Ok(guard)
                    }).map_err(mailbox_api_error)?;
                    federation_mailbox_response(central.clone(), &FederationDeliveryTerminal {
                        state: FederationDeliveryTerminalState::Refused,
                        canonical_digest: delivery.digest(),
                    })
                }
                Err(error) => Err(error),
            }
        })();
        match attempted {
            Ok(response) => Ok(response),
            Err(error) => {
                // Another append/cancel may have won after the first lookup.
                // Recover that outcome under sender-only authority as well.
                match self.mailbox_store.retained_federation_delivery(
                    &delivery, || sender_authority.lock(),
                ).map_err(mailbox_api_error)? {
                    Some(outcome) => federation_mailbox_response(central, &outcome),
                    None => Err(error),
                }
            }
        }
    }
}

fn federation_delivery_target_retired_locked(
    inner: &StateInner,
    binding: &FederationMailboxBinding,
) -> bool {
    if binding.identity.host_id == binding.central.host_id {
        // Enrollment assigns a domain creation independently of the native
        // creation UUID. Compare domain identities, including for hidden roots;
        // a present but unbound root does not prove the old creation retired.
        return inner.find_session_index(&binding.identity.session_id).is_none_or(|index| {
            inner.sessions[index].mailbox_binding()
                .is_some_and(|current| current.identity != binding.identity)
        });
    }
    // Link loss, re-enrollment and missing directory/placement rows are not
    // evidence of retirement. Only the exact completed placement certifies it.
    inner.federation.placements.values().any(|placement| {
        placement.identity == binding.identity
            && placement.placement_generation == binding.placement_generation
            && placement.peer.local == binding.central
            && placement.state == FederationPlacementState::Completed
    })
}

impl MailboxStore {
    fn retained_federation_delivery<G>(
        &self,
        delivery: &FederationDelivery,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<Option<FederationDeliveryOutcome>> {
        let outcome = {
            let connection = self.connection()?;
            let sender = delivery.sender.participant_key();
            let retired: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM federation_mailbox_retired_namespaces WHERE sender_session_id=?1)",
                [&sender], |row| row.get(0),
            )?;
            if retired { return Err(federation_delivery_conflict()); }
            let row: Option<(String, String, String, Option<String>, Option<u64>)> = connection.query_row(
                "SELECT canonical_digest,target_session_id,state,message_id,sequence
                 FROM federation_mailbox_dedup WHERE sender_host_id=?1 AND sender_session_id=?2 AND idempotency_key=?3",
                rusqlite::params![delivery.sender.identity.host_id, sender, delivery.key],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
            ).optional()?;
            let outcome = match row {
                None => None,
                Some((digest, target, state, message_id, sequence)) => {
                    if digest != delivery.digest() || target != delivery.target.participant_key() {
                        return Err(federation_delivery_conflict());
                    }
                    Some(match state.as_str() {
                        "homeCommitted" => {
                            let message = mailbox_message_for_idempotency_key(&connection, &sender, &delivery.key)?
                                .ok_or_else(|| anyhow!("retained committed delivery is missing its message"))?;
                            if message_id.as_deref() != Some(message.message_id.as_str())
                                || sequence != Some(message.sequence) || message.target_session_id != target
                            { return Err(federation_delivery_conflict()); }
                            let cursor = mailbox_processed_through(&connection, &message.mailbox_id, &sender)?;
                            FederationDeliveryOutcome::Committed(MailboxAppendReceipt {
                                notification_cause: (delivery.target.identity.host_id != delivery.target.central.host_id)
                                    .then(|| "duplicate delivery; original notification decision retained".to_owned()),
                                mailbox_id: message.mailbox_id, message_id: message.message_id,
                                sequence: message.sequence, unread_depth: message.unread_depth_at_append,
                                notification_disposition: "durableButNotWoken".to_owned(),
                                duplicate: true, sender_processed_through: cursor, sender_cursor_advanced: false,
                            })
                        }
                        "refused" | "abandoned" => FederationDeliveryOutcome::Terminal(FederationDeliveryTerminal {
                            state: if state == "refused" { FederationDeliveryTerminalState::Refused }
                                else { FederationDeliveryTerminalState::Abandoned },
                            canonical_digest: digest,
                        }),
                        _ => return Err(federation_delivery_conflict()),
                    })
                }
            };
            let _sender = authorize()?;
            outcome
        };
        // The immutable commitment is already known. Wake completion is a
        // separate projection, and no duplicate starts or waits for delivery.
        Ok(outcome)
    }
}
