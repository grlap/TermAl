// Owns commit-adjacent domain mailbox authorization under link -> state locks.
// Does not wait for storage admission or perform transport. New adapter between
// mailboxes.rs transaction boundaries and M1/L1's exact authority witnesses.

struct FederationMailboxAuthority {
    app: AppState,
    actor: FederationMailboxBinding,
    lease: Option<FederationLinkLease>,
    target: Option<FederationMailboxParticipant>,
}

struct FederationMailboxAdmission<'a> {
    _inner: StateMutexGuard<'a, StateInner>,
    _link: Option<std::sync::MutexGuard<'a, FederationLinkState>>,
}

impl FederationMailboxAuthority {
    /// Called ONLY after acquiring the SQLite connection/transaction. Neither
    /// the storage admission wait nor any network I/O occurs under these locks.
    fn lock(&self) -> Result<FederationMailboxAdmission<'_>> {
        let link = self
            .lease
            .as_ref()
            .map(|lease| {
                let link = lease
                    .channel
                    .state
                    .lock()
                    .expect("federation link mutex poisoned");
                if link.incarnation.as_deref() != Some(lease.incarnation.as_str())
                    || link.recorded_incarnation.as_deref() != Some(lease.incarnation.as_str())
                {
                    return Err(mailbox_store_error(
                        MailboxStoreErrorKind::Conflict,
                        FEDERATION_STALE_AUTHORITY,
                    ));
                }
                Ok(link)
            })
            .transpose()?;
        let inner = self.app.inner.lock().expect("state mutex poisoned");
        let stale =
            || mailbox_store_error(MailboxStoreErrorKind::Conflict, FEDERATION_STALE_AUTHORITY);
        let central = federation_mailbox_central_locked(&inner).map_err(|_| stale())?;
        if central != self.actor.central {
            return Err(stale());
        }
        if let Some(lease) = &self.lease {
            let basis = lease.channel.transport_basis();
            let peer = self
                .app
                .refresh_federation_transport_snapshot_locked(&inner, &basis, &lease.incarnation)
                .map_err(|_| stale())?;
            if !federation_mailbox_remote_in_domain(
                &inner.federation,
                &inner.preferences.federation,
                &peer,
                &self.actor,
            ) {
                return Err(stale());
            }
        }
        let actor = inner
            .federation
            .mailbox_directory
            .get(&self.actor.participant_key())
            .ok_or_else(stale)?;
        if actor.binding != self.actor
            || !federation_mailbox_participant_current_locked(&inner, actor)
            || self.target.as_ref().is_some_and(|target| {
                inner
                    .federation
                    .mailbox_directory
                    .get(&target.session_id)
                    .is_none_or(|current| current.binding != target.binding)
                    || !federation_mailbox_participant_current_locked(&inner, target)
            })
        {
            return Err(stale());
        }
        Ok(FederationMailboxAdmission {
            _link: link,
            _inner: inner,
        })
    }
}
