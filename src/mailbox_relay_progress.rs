// Read-only native relay observations, split from the exact-message projection
// in mailbox_current_status.rs and the relay routes/RPC. Owns no queue mutation,
// receipt, acknowledgement, retry, provider dispatch or custody authority.

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct MailboxRelayProgress {
    home_observed_at: String,
    // The home's delivered bit records target queue admission, not a turn.
    home_queue_admission_confirmed: Option<bool>,
    target: MailboxRelayTargetProgress,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "availability",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum MailboxRelayTargetProgress {
    Observed {
        observed_at: String,
        // None means this target has no retained notification observation.
        queue_admission_recorded: Option<bool>,
        provider_handoff_recorded: Option<bool>,
        currently_queued: bool,
        current_session_holds: Vec<MailboxRelayTargetHold>,
        recovery: String,
    },
    Unavailable {
        observed_at: String,
        reason: String,
        recovery: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum MailboxRelayTargetHold {
    ActiveTurn,
    Approval,
    Stopping,
    Paused,
    ProviderCapacity,
    ResourceReset,
    Durability,
    UnsettledDelivery,
    HumanInput,
    WakeAuthority,
}

fn mailbox_relay_progress_recovery() -> String {
    "Read the recipient mailbox before acknowledging its issued receipt. Do not resend a saved message to wake it. An active turn must finish; resolve a pending approval, provider capacity or explicit Stop/pause through its existing controls. Durable relay recovery retries when the authenticated link and ordinary queue are available; an exact status read does not release a hold or execute work.".into()
}

fn mailbox_relay_target_unavailable(reason: &str) -> MailboxRelayTargetProgress {
    MailboxRelayTargetProgress::Unavailable {
        observed_at: chrono::Utc::now().to_rfc3339(),
        reason: mailbox_relay_bounded_reason(reason),
        recovery: mailbox_relay_progress_recovery(),
    }
}

impl MailboxStore {
    fn attach_mailbox_relay_home_progress(&self, message: &mut MailboxMessage) -> Result<()> {
        let connection = self.connection()?;
        let admitted = connection.query_row(
            "SELECT delivered FROM mailbox_relay_wakes WHERE message_id=?1 AND mailbox_id=?2 AND through_sequence=?3",
            rusqlite::params![message.id, message.mailbox_id, message.sequence],
            |row| row.get::<_, bool>(0),
        ).optional()?;
        if let Some(status) = message.current_status.as_mut() {
            let progress = MailboxRelayProgress {
                home_observed_at: chrono::Utc::now().to_rfc3339(),
                home_queue_admission_confirmed: admitted,
                target: mailbox_relay_target_unavailable("target progress has not been observed"),
            };
            status.summary = mailbox_relay_current_status_summary(
                &status.observed_at,
                &progress,
                status.recipient_acknowledged,
            );
            status.relay_progress = Some(progress);
        }
        Ok(())
    }
}

impl AppState {
    fn observe_native_mailbox_relay_target(
        &self,
        actor: &MailboxRelayPrincipal,
        conversation: &MailboxRelayConversation,
        target: &MailboxRelayPrincipal,
        mailbox_id: &str,
        sequence: u64,
        lease: Option<&FederationLinkLease>,
    ) -> Result<MailboxRelayTargetProgress, ApiError> {
        conversation.require_member(actor)?;
        conversation.require_member(target)?;
        if sequence == 0 || !valid_mailbox_relay_id(mailbox_id) {
            return Err(ApiError::bad_request(
                "invalid native mailbox progress boundary",
            ));
        }
        if self
            .native_mailbox_relay_descriptor(&target.native_session_id)?
            .principal
            != *target
        {
            return Err(ApiError::conflict(
                "native mailbox progress target was replaced",
            ));
        }
        self.require_mailbox_relay_conversation(target, mailbox_id, conversation)?;
        let notification = self
            .mailbox_store
            .mailbox_relay_notification(target, mailbox_id, sequence)
            .map_err(mailbox_api_error)?;
        if notification
            .as_ref()
            .is_some_and(|notification| notification.wake.conversation != *conversation)
        {
            return Err(ApiError::conflict(
                "native mailbox progress conversation changed",
            ));
        }
        let locator = MailboxRelayLocator::encode(&conversation.home, mailbox_id, false)?;
        let wake_authority =
            notification
                .as_ref()
                .map(|notification| MailboxRelayDispatchAuthority {
                    app: self.clone(),
                    wake: notification.wake.clone(),
                    peer: notification.peer.clone(),
                    lease: None,
                });
        let observe = |inner: &StateInner| -> Result<MailboxRelayTargetProgress, ApiError> {
            let root = federation_mailbox_root_locked(inner, &target.native_session_id)?;
            if inner.federation.local.as_ref() != Some(&target.host)
                || root.federation_native_creation_id != target.native_creation_id
            {
                return Err(ApiError::conflict("native mailbox progress target changed"));
            }
            let mut holds = Vec::new();
            match root.session.status {
                SessionStatus::Active => holds.push(MailboxRelayTargetHold::ActiveTurn),
                SessionStatus::Approval => holds.push(MailboxRelayTargetHold::Approval),
                SessionStatus::Stopping => holds.push(MailboxRelayTargetHold::Stopping),
                _ => {}
            }
            if root.runtime_stop_in_progress && !holds.contains(&MailboxRelayTargetHold::Stopping) {
                holds.push(MailboxRelayTargetHold::Stopping);
            }
            if root.orchestrator_auto_dispatch_blocked || root.engram.operator_paused {
                holds.push(MailboxRelayTargetHold::Paused);
            }
            if root.provider_capacity_retry.is_some() || root.session.provider_capacity_waiting {
                holds.push(MailboxRelayTargetHold::ProviderCapacity);
            }
            if session_resource_reset_in_progress(inner, root) {
                holds.push(MailboxRelayTargetHold::ResourceReset);
            }
            if let Some(next) =
                ordinary_queue_position(root).and_then(|position| root.queued_prompts.get(position))
            {
                if queued_prompt_held_for_durability(root, next) {
                    holds.push(MailboxRelayTargetHold::Durability);
                }
                if next.source == QueuedPromptSource::Human {
                    holds.push(MailboxRelayTargetHold::HumanInput);
                }
            }
            if wake_authority
                .as_ref()
                .is_some_and(|authority| authority.validate_locked(inner).is_err())
            {
                holds.push(MailboxRelayTargetHold::WakeAuthority);
            }
            let currently_queued = root.queued_prompts.iter().any(|queued| {
                queued
                    .pending_prompt
                    .source
                    .as_ref()
                    .and_then(|source| source.mailbox.as_ref())
                    .is_some_and(|mailbox| {
                        mailbox.mailbox_id == locator && mailbox.sequence >= sequence
                    })
            });
            if root.queued_prompts.iter().any(|queued| {
                !ordinary_queue_entry_is_runnable(root, queued)
                    && queued
                        .pending_prompt
                        .source
                        .as_ref()
                        .and_then(|source| source.mailbox.as_ref())
                        .is_some_and(|mailbox| {
                            mailbox.mailbox_id == locator && mailbox.sequence >= sequence
                        })
            }) {
                holds.push(MailboxRelayTargetHold::UnsettledDelivery);
            }
            Ok(MailboxRelayTargetProgress::Observed {
                observed_at: chrono::Utc::now().to_rfc3339(),
                queue_admission_recorded: notification
                    .as_ref()
                    .map(|notification| notification.queued),
                provider_handoff_recorded: notification
                    .as_ref()
                    .map(|notification| notification.handed_off),
                currently_queued,
                current_session_holds: holds,
                recovery: mailbox_relay_progress_recovery(),
            })
        };
        // Read SQL before the authority/state lock; no I/O or await under it.
        if let Some(lease) = lease {
            let authority = MailboxRelayAuthority {
                app: self.clone(),
                lease: lease.clone(),
                native: vec![target.clone()],
                home: None,
            };
            let guard = authority.lock().map_err(mailbox_api_error)?;
            observe(&guard._inner)
        } else {
            let inner = self.inner.lock().expect("state mutex poisoned");
            observe(&inner)
        }
    }

    async fn attach_native_mailbox_relay_progress(
        &self,
        actor: &MailboxRelayPrincipal,
        home: &MailboxRelayHome,
        conversations: &[MailboxRelayConversation],
        message: &mut MailboxMessage,
    ) {
        // ReadMessage returns home-native participant keys, not public locators.
        // Several conversations can share one home; select this exact pair.
        let observation = async {
            let conversation = conversations
                .iter()
                .find(|conversation| {
                    conversation.home == *home
                        && conversation.participants.iter().any(|principal| {
                            principal.participant_key() == message.sender_session_id
                        })
                        && conversation.participants.iter().any(|principal| {
                            principal.participant_key() == message.target_session_id
                        })
                })
                .ok_or_else(|| {
                    ApiError::not_found("exact native mailbox progress membership unavailable")
                })?;
            conversation.require_member(actor)?;
            let target = conversation
                .participants
                .iter()
                .find(|principal| principal.participant_key() == message.target_session_id)
                .ok_or_else(|| ApiError::not_found("native mailbox progress target unavailable"))?;
            if target.host == actor.host {
                let app = self.clone();
                let actor = actor.clone();
                let conversation = conversation.clone();
                let target = target.clone();
                let mailbox_id = message.mailbox_id.clone();
                let sequence = message.sequence;
                run_blocking_api(move || {
                    app.observe_native_mailbox_relay_target(
                        &actor,
                        &conversation,
                        &target,
                        &mailbox_id,
                        sequence,
                        None,
                    )
                })
                .await
            } else {
                let (handle, lease, _) = self.mailbox_relay_peer(actor, conversation)?;
                self.request_mailbox_relay(
                    &handle,
                    &lease,
                    MailboxRelayOperation::InspectWake {
                        actor: actor.clone(),
                        conversation: conversation.clone(),
                        target: target.clone(),
                        mailbox_id: message.mailbox_id.clone(),
                        sequence: message.sequence,
                    },
                )
                .await
            }
        }
        .await;
        if let Some(status) = message.current_status.as_mut()
            && let Some(progress) = status.relay_progress.as_mut()
        {
            progress.target = match observation {
                Ok(observed) => observed,
                Err(error) => mailbox_relay_target_unavailable(&error.message),
            };
            status.summary = mailbox_relay_current_status_summary(
                &status.observed_at,
                progress,
                status.recipient_acknowledged,
            );
        }
    }
}
