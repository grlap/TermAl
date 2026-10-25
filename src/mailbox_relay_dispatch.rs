// Immutable enrollment evidence and current authority for metadata-only relay
// wake promotion/provider admission. History access has a separate authority.
#[cfg(test)]
std::thread_local! {
    static TEST_MAILBOX_RELAY_AFTER_EVALUATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static TEST_MAILBOX_RELAY_AFTER_SEND_REVALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// An expected scheduling hold, not a provider/protocol or persistence failure.
/// Generic drains consume only this classification; genuine failures propagate.
#[derive(Debug)]
struct MailboxRelayAdmissionHeld(String);

impl std::fmt::Display for MailboxRelayAdmissionHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for MailboxRelayAdmissionHeld {}

fn mailbox_relay_admission_held(error: impl std::fmt::Display) -> anyhow::Error {
    MailboxRelayAdmissionHeld(error.to_string()).into()
}

fn consume_mailbox_relay_hold<T>(result: Result<Option<T>>) -> Result<Option<T>> {
    match result {
        Err(error) if error.downcast_ref::<MailboxRelayAdmissionHeld>().is_some() => {
            eprintln!("native relay> queue remains held: {error:#}");
            Ok(None)
        }
        result => result,
    }
}

#[derive(Clone)]
struct MailboxRelayDrainOwner {
    queue: EngramQueuedAdmissionOwner,
    mailbox: String,
    message: String,
    sequence: u64,
    stop_generation: u64,
    pause_epoch: u64,
    signal: Arc<FederationExecutorWakeSignal>,
}

impl MailboxRelayDrainOwner {
    fn capture(record: &SessionRecord, wake: &MailboxRelayWake, signal: Arc<FederationExecutorWakeSignal>) -> Option<Self> {
        Self::capture_boundary(record, &wake.conversation.home, &wake.mailbox_id, wake.sequence, signal)
    }

    fn capture_boundary(record: &SessionRecord, home: &MailboxRelayHome, mailbox: &str, sequence: u64,
        signal: Arc<FederationExecutorWakeSignal>) -> Option<Self> {
        let queue = EngramQueuedAdmissionOwner::capture(record)?;
        let source = record.queued_prompts.front()?.pending_prompt.source.as_ref()?.mailbox.as_ref()?;
        let locator = MailboxRelayLocator::decode(&source.mailbox_id, false).ok()??;
        if source.sequence < sequence || locator.id != mailbox || &locator.home != home {
            return None;
        }
        Some(Self {
            queue, mailbox: source.mailbox_id.clone(), message: source.message_id.clone(),
            sequence: source.sequence, stop_generation: record.runtime_stop_generation,
            pause_epoch: record.auto_dispatch_pause_epoch, signal,
        })
    }

    fn matches(&self, record: &SessionRecord) -> bool {
        !self.signal.is_stopping() && self.queue.matches(record)
            && record.runtime_stop_generation == self.stop_generation
            && record.auto_dispatch_pause_epoch == self.pause_epoch
            && record.queued_prompts.front().and_then(|queued| queued.pending_prompt.source.as_ref())
                .and_then(|source| source.mailbox.as_ref()).is_some_and(|source|
                    source.mailbox_id == self.mailbox && source.message_id == self.message && source.sequence == self.sequence)
    }
}

fn validate_mailbox_relay_wake_peer(
    wake: &MailboxRelayWake,
    peer: &FederationPeerSnapshot,
    home: bool,
) -> Result<()> {
    wake.validate()
        .map_err(|error| relay_store_error(&error.message))?;
    peer.validate()
        .map_err(|error| relay_store_error(&error.message))?;
    if peer.local
        != if home {
            wake.conversation.home.host.clone()
        } else {
            wake.target.host.clone()
        }
        || peer.central != wake.conversation.home.host
        || !wake
            .conversation
            .participants
            .iter()
            .any(|p| p.host == peer.local)
        || !wake
            .conversation
            .participants
            .iter()
            .any(|p| p.host == peer.peer)
    {
        return Err(relay_store_error(
            "native relay wake enrollment does not own this boundary",
        ));
    }
    Ok(())
}

struct MailboxRelayDispatchAuthority {
    app: AppState,
    wake: MailboxRelayWake,
    peer: FederationPeerSnapshot,
    lease: Option<FederationLinkLease>,
}

impl MailboxRelayDispatchAuthority {
    fn lock_link(&self) -> Result<Option<std::sync::MutexGuard<'_, FederationLinkState>>> {
        self.lease.as_ref().map(|lease| {
            let link = lease.channel.state.lock().expect("federation link mutex poisoned");
            if link.incarnation.as_deref() != Some(&lease.incarnation)
                || link.recorded_incarnation.as_deref() != Some(&lease.incarnation) {
                return Err(relay_store_error("native relay wake link changed; obligation held"));
            }
            Ok(link)
        }).transpose()
    }

    fn validate_locked(&self, inner: &StateInner) -> Result<(), ApiError> {
        let current = federation_snapshot_locked(inner, &self.peer.peer.host_id)?;
        if current.membership != FederationMembershipState::Active
            || !same_mailbox_relay_enrollment(&self.peer, &current)
            || inner.federation.admission_quarantine.is_some()
        {
            return Err(ApiError::conflict(
                "native relay wake authority changed; obligation held",
            ));
        }
        let target = &self.wake.target;
        let root = federation_mailbox_root_locked(inner, &target.native_session_id)?;
        if inner.federation.local.as_ref() != Some(&target.host)
            || root.federation_native_creation_id != target.native_creation_id
        {
            return Err(ApiError::conflict(
                "native relay wake owner changed; obligation held",
            ));
        }
        if let Some(lease) = &self.lease {
            let current = self.app.refresh_federation_transport_snapshot_locked(
                inner,
                &lease.channel.transport_basis(),
                &lease.incarnation,
            )?;
            if !same_mailbox_relay_enrollment(&self.peer, &current) {
                return Err(federation_stale_link());
            }
        }
        Ok(())
    }

    fn lock(&self) -> Result<FederationMailboxAdmission<'_>> {
        let link = self.lock_link()?;
        let inner = self.app.inner.lock().expect("state mutex poisoned");
        self.validate_locked(&inner)
            .map_err(|error| relay_store_error(&error.message))?;
        Ok(FederationMailboxAdmission {
            _inner: inner,
            _link: link,
        })
    }
}

impl AppState {
    fn prepare_front_mailbox_relay_authority(
        &self,
        session: &str,
    ) -> Result<Option<(String, MailboxRelayDispatchAuthority)>, ApiError> {
        let front = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session) else {
                return Ok(None);
            };
            inner.sessions[index]
                .queued_prompts
                .front()
                .and_then(|queued| {
                    let source = queued.pending_prompt.source.as_ref()?.mailbox.as_ref()?;
                    source.mailbox_id.starts_with("relay-mailbox-").then(|| {
                        (
                            queued.pending_prompt.id.clone(),
                            MailboxNotificationDelivery {
                                session_id: session.to_owned(),
                                mailbox_id: source.mailbox_id.clone(),
                                through_sequence: source.sequence,
                            },
                        )
                    })
                })
        };
        let Some((id, notification)) = front else {
            return Ok(None);
        };
        let authority = self
            .prepare_mailbox_relay_dispatch_authority(&notification)?
            .ok_or_else(|| ApiError::conflict("native relay head authority missing"))?;
        Ok(Some((id, authority)))
    }

    fn validate_front_mailbox_relay_authority_locked(
        &self,
        inner: &StateInner,
        session: &str,
        captured: Option<&(String, MailboxRelayDispatchAuthority)>,
    ) -> Result<(), ApiError> {
        let Some(root) = inner
            .find_session_index(session)
            .map(|index| &inner.sessions[index])
        else {
            return Ok(());
        };
        let Some(queued) = root.queued_prompts.front() else {
            return Ok(());
        };
        let Some(source) = queued
            .pending_prompt
            .source
            .as_ref()
            .and_then(|source| source.mailbox.as_ref())
            .filter(|source| source.mailbox_id.starts_with("relay-mailbox-"))
        else {
            return Ok(());
        };
        let Some((id, authority)) = captured.filter(|(id, _)| *id == queued.pending_prompt.id)
        else {
            return Err(ApiError::conflict(
                "native relay queue head changed; retry admission",
            ));
        };
        let _ = id;
        if source.sequence != authority.wake.sequence
            || MailboxRelayLocator::decode(&source.mailbox_id, false)?.is_none_or(|locator| {
                locator.home != authority.wake.conversation.home
                    || locator.id != authority.wake.mailbox_id
            })
        {
            return Err(ApiError::conflict("native relay queue boundary changed"));
        }
        authority.validate_locked(inner)
    }

    fn prepare_mailbox_relay_dispatch_authority(
        &self,
        notification: &MailboxNotificationDelivery,
    ) -> Result<Option<MailboxRelayDispatchAuthority>, ApiError> {
        let Some(locator) = MailboxRelayLocator::decode(&notification.mailbox_id, false)? else {
            return Ok(None);
        };
        let actor = self
            .native_mailbox_relay_descriptor(&notification.session_id)?
            .principal;
        let retained = self
            .mailbox_store
            .mailbox_relay_notification(&actor, &locator.id, notification.through_sequence)
            .map_err(mailbox_api_error)?
            .ok_or_else(|| {
                ApiError::conflict("native relay wake metadata missing; obligation held")
            })?;
        if retained.wake.conversation.home != locator.home {
            return Err(ApiError::conflict("native relay wake home changed"));
        }
        self.mailbox_relay_dispatch_authority(&retained.wake, retained.peer, None)
            .map(Some)
    }

    fn mailbox_relay_dispatch_authority(
        &self,
        wake: &MailboxRelayWake,
        peer: FederationPeerSnapshot,
        incoming: Option<&FederationLinkLease>,
    ) -> Result<MailboxRelayDispatchAuthority, ApiError> {
        validate_mailbox_relay_wake_peer(wake, &peer, false).map_err(mailbox_api_error)?;
        if self
            .native_mailbox_relay_descriptor(&wake.target.native_session_id)?
            .principal
            != wake.target
        {
            return Err(ApiError::conflict("native relay wake target/store changed"));
        }
        let lease = if wake.conversation.home.host == wake.target.host {
            None
        } else {
            Some(match incoming {
                Some(lease) => lease.clone(),
                None => self
                    .federation_peer_handle(&peer.peer.host_id)
                    .ok_or_else(|| {
                        ApiError::conflict("native relay home unavailable; obligation held")
                    })?
                    .channel
                    .current_lease()?,
            })
        };
        let authority = MailboxRelayDispatchAuthority {
            app: self.clone(),
            wake: wake.clone(),
            peer,
            lease,
        };
        {
            let _guard = authority.lock().map_err(mailbox_api_error)?;
        }
        Ok(authority)
    }

    fn mailbox_relay_home_wake_peer(
        &self,
        wake: &MailboxRelayWake,
    ) -> Result<FederationPeerSnapshot, ApiError> {
        let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
        let json: String = connection.query_row("SELECT peer_json FROM mailbox_relay_wakes WHERE message_id=?1 AND mailbox_id=?2 AND principal_key=?3 AND through_sequence=?4",
            rusqlite::params![wake.message_id, wake.mailbox_id, wake.target.participant_key(), wake.sequence], |row|row.get(0))
            .map_err(|error|mailbox_api_error(error.into()))?;
        let peer: FederationPeerSnapshot = serde_json::from_str(&json)
            .map_err(|_| ApiError::conflict("native relay home wake evidence malformed"))?;
        validate_mailbox_relay_wake_peer(wake, &peer, true).map_err(mailbox_api_error)?;
        let current = self.federation_peer_snapshot(&peer.peer.host_id)?;
        if current.membership != FederationMembershipState::Active
            || !same_mailbox_relay_enrollment(&peer, &current)
        {
            return Err(ApiError::conflict(
                "native relay home wake enrollment changed; obligation held",
            ));
        }
        Ok(peer)
    }
}
