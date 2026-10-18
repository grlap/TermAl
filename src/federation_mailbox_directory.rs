// Owns explicit participant registration and current qualified-identity checks.
// Does not adopt standalone sessions, authenticate transport, or store messages.
// New directory over M1's durable federation state and session placement.

impl AppState {
    /// Internal local wake adapters may read C's store for a bound C-native
    /// session. A satellite binding never authorizes access to its local store.
    fn local_mailbox_storage_key(&self, session_id: &str) -> Option<String> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = federation_mailbox_root_locked(&inner, session_id).ok()?;
        match &record.federation_mailbox_binding {
            None => Some(session_id.to_owned()),
            Some(binding) => inner
                .federation
                .mailbox_directory
                .get(&binding.participant_key())
                .filter(|row| {
                    row.binding == *binding
                        && federation_mailbox_participant_current_locked(&inner, row)
                })
                .map(|_| binding.participant_key()),
        }
    }
    fn register_federation_mailbox_actor(
        &self,
        peer: &FederationPeerSnapshot,
        actor: &FederationMailboxActor,
    ) -> Result<FederationMailboxParticipant, ApiError> {
        actor.binding.validate()?;
        let participant = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            self.ensure_federation_peer_current_locked(&inner, peer)?;
            federation_mailbox_central_locked(&inner)?;
            if !federation_mailbox_remote_in_domain(
                &inner.federation,
                &inner.preferences.federation,
                peer,
                &actor.binding,
            ) {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let participant = FederationMailboxParticipant {
                binding: actor.binding.clone(),
                session_id: actor.binding.participant_key(),
                name: federation_mailbox_display_name(&actor.name),
                host_name: inner
                    .federation
                    .peers
                    .get(&peer.peer.host_id)
                    .expect("peer checked")
                    .display_name
                    .clone(),
            };
            participant.validate()?;
            if inner
                .federation
                .mailbox_directory
                .get(&participant.session_id)
                == Some(&participant)
            {
                return Ok(participant);
            }
            participant
        };
        self.mutate_federation(|domain, settings| {
            if !federation_mailbox_remote_in_domain(domain, settings, peer, &actor.binding) {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            domain
                .mailbox_directory
                .insert(participant.session_id.clone(), participant.clone());
            Ok(participant.clone())
        })
    }

    fn federation_mailbox_actor(
        &self,
        session_id: &str,
    ) -> Result<Option<FederationMailboxActor>, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = federation_mailbox_root_locked(&inner, session_id)?;
        let Some(binding) = &record.federation_mailbox_binding else {
            return Ok(None);
        };
        if !inner.preferences.federation.enabled
            || !inner.federation_durable
            || inner.federation_continuity_lost
            || inner.federation.admission_quarantine.is_some()
            || inner.preferences.federation.central.as_ref() != Some(&binding.central)
            || inner.federation.local.as_ref().map(|host| &host.host_id)
                != Some(&binding.identity.host_id)
            || binding.identity.session_id != session_id
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(Some(FederationMailboxActor {
            binding: binding.clone(),
            name: federation_mailbox_display_name(&record.session.name),
        }))
    }
}
