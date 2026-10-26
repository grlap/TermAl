// Owns explicit participant registration and current qualified-identity checks.
// Does not adopt standalone sessions, authenticate transport, or store messages.
// New directory over M1's durable federation state and session placement.

impl AppState {
    /// Native history keeps its original principal after binding. Only a
    /// current C-native binding adds a second principal in this local store.
    fn local_mailbox_storage_keys(&self, session_id: &str) -> Vec<String> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let Ok(record) = federation_mailbox_root_locked(&inner, session_id) else { return Vec::new(); };
        let mut keys = vec![session_id.to_owned()];
        if let Some(binding) = record.mailbox_binding() {
            if binding.identity.host_id == binding.central.host_id && inner
                .federation
                .mailbox_directory
                .get(&binding.participant_key())
                .is_some_and(|row| {
                    row.binding == *binding
                        && federation_mailbox_participant_current_locked(&inner, row)
                })
            { keys.push(binding.participant_key()); }
        }
        keys
    }

    /// Choose by durable membership in this mailbox, never by alias or a
    /// root's new routing namespace. Store I/O happens after releasing state.
    fn local_mailbox_storage_key(&self, session_id: &str, mailbox_id: &str) -> Result<Option<String>> {
        let keys = self.local_mailbox_storage_keys(session_id);
        let Some(connection) = self.mailbox_store.connection_if_enabled() else { return Ok(None); };
        for key in keys {
            let owns = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM mailbox_participants WHERE mailbox_id=?1 AND session_id=?2)",
                rusqlite::params![mailbox_id, key], |row| row.get::<_, bool>(0),
            ).context("failed to select local mailbox wake owner")?;
            if owns { return Ok(Some(key)); }
        }
        Ok(None)
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
        self.mutate_federation_core_on_link_with_budget(None, false, |domain, settings, _| {
            if !federation_mailbox_remote_in_domain(domain, settings, peer, &actor.binding) {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            domain
                .mailbox_directory
                .insert(participant.session_id.clone(), participant.clone());
            Ok(participant.clone())
        }, |_| {}, EngramBudgetClock::Real, Duration::from_secs(2))
        .map_err(|error| {
            if error.status.is_server_error() {
                error.with_kind(ApiErrorKind::FederationMailboxDirectoryFence)
            } else { error }
        })
    }

    fn federation_mailbox_actor(
        &self,
        session_id: &str,
    ) -> Result<Option<FederationMailboxActor>, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = federation_mailbox_root_locked(&inner, session_id)?;
        let Some(binding) = record.mailbox_binding() else {
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
