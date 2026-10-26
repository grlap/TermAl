// Owns commit-fenced explicit native and satellite participant bindings.
// Does not discover aliases or copy mailbox data. Split from
// federation_mailbox_directory.rs while adding central mailbox participation.

impl AppState {
    /// Local operator/API opt-in. No other session is enrolled by this action.
    fn join_federation_mailbox_session(
        &self,
        session_id: &str,
    ) -> Result<FederationMailboxParticipant, ApiError> {
        self.join_federation_mailbox_session_with_history(session_id, false)
    }

    fn join_federation_mailbox_session_with_history(
        &self,
        session_id: &str,
        preserve_history: bool,
    ) -> Result<FederationMailboxParticipant, ApiError> {
        // Already-admitted reads do not wait for the append writer or query
        // standalone history. First admission revalidates again under writer.
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            federation_mailbox_central_locked(&inner)?;
            let root = federation_mailbox_root_locked(&inner, session_id)?;
            if let Some(binding) = root.mailbox_binding() {
                return inner.federation.mailbox_directory.get(&binding.participant_key())
                    .filter(|row| row.binding == *binding && federation_mailbox_participant_current_locked(&inner, row))
                    .cloned().ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
        }
        let namespace_writer = self
            .mailbox_store
            .lock_writer("checking standalone mailbox history before binding")
            .map_err(mailbox_api_error)?;
        let history = self
            .mailbox_store
            .standalone_mailbox_history(session_id)
            .map_err(mailbox_api_error)?;
        let (participant, image, dispatch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let central = federation_mailbox_central_locked(&inner)?;
            let record = federation_mailbox_root_locked(&inner, session_id)?;
            if let Some(binding) = record.mailbox_binding() {
                let existing = inner
                    .federation
                    .mailbox_directory
                    .get(&binding.participant_key())
                    .filter(|row| {
                        row.binding == *binding
                            && federation_mailbox_participant_current_locked(&inner, row)
                    })
                    .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
                return Ok(existing.clone());
            }
            if !preserve_history {
                refuse_standalone_mailbox_history(&history)?;
            }
            let binding = FederationMailboxBinding {
                identity: FederationSessionIdentity {
                    host_id: central.host_id.clone(),
                    session_id: session_id.to_owned(),
                    creation_id: Uuid::new_v4().to_string(),
                },
                central,
                placement_generation: 1,
            };
            let participant = FederationMailboxParticipant {
                session_id: binding.participant_key(),
                binding,
                host_name: "Central".to_owned(),
                name: federation_mailbox_display_name(&record.session.name),
            };
            participant.validate()?;
            let index = inner.find_session_index(session_id).expect("root checked");
            let root = inner.session_mut_by_index(index).expect("root checked");
            if preserve_history {
                root.federation_automatic_mailbox_binding = Some(participant.binding.clone());
            } else {
                root.federation_mailbox_binding = Some(participant.binding.clone());
            }
            inner
                .federation
                .mailbox_directory
                .insert(participant.session_id.clone(), participant.clone());
            inner.federation.version = federation_next_generation(inner.federation.version)?;
            inner.federation_durable = false;
            let image = federation_persist_image(&PersistedState::metadata_from_inner(&inner));
            let dispatch = self
                .persist_internal_locked_with_dispatch(&inner)
                .map_err(|_| {
                    ApiError::internal(
                        "mailbox enrollment persistence failed; admission remains held",
                    )
                })?;
            (participant, image, dispatch)
        };
        drop(namespace_writer);
        // The unique domain version and session binding are persisted by the
        // same state transaction; do not expose the binding until it settles.
        self.finish_federation_durability_with_clock(&image, dispatch, EngramBudgetClock::Real)?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        if !federation_mailbox_participant_current_locked(&inner, &participant) {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(participant)
    }

    /// Routing-only installation by an authenticated central command, not an
    /// agent payload. The caller supplies the exact current enrolled channel.
    #[cfg(test)]
    fn install_federation_mailbox_binding(
        &self,
        peer: &FederationPeerSnapshot,
        binding: FederationMailboxBinding,
        expected_native_creation_id: &str,
    ) -> Result<(), ApiError> {
        self.install_federation_mailbox_binding_on_link(
            peer,
            binding,
            expected_native_creation_id,
            None,
        )
    }

    fn install_federation_mailbox_binding_on_link(
        &self,
        peer: &FederationPeerSnapshot,
        binding: FederationMailboxBinding,
        expected_native_creation_id: &str,
        lease: Option<&FederationLinkLease>,
    ) -> Result<(), ApiError> {
        self.install_federation_mailbox_binding_preserving_history(
            peer, binding, expected_native_creation_id, lease, false,
        )
    }

    fn install_federation_mailbox_binding_preserving_history(
        &self,
        peer: &FederationPeerSnapshot,
        binding: FederationMailboxBinding,
        expected_native_creation_id: &str,
        lease: Option<&FederationLinkLease>,
        preserve_history: bool,
    ) -> Result<(), ApiError> {
        binding.validate()?;
        let namespace_writer = self
            .mailbox_store
            .lock_writer("checking standalone mailbox history before binding")
            .map_err(mailbox_api_error)?;
        let history = self
            .mailbox_store
            .standalone_mailbox_history(&binding.identity.session_id)
            .map_err(mailbox_api_error)?;
        let install = || {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(lease) = lease {
                self.refresh_federation_transport_snapshot_locked(
                    &inner,
                    &lease.channel.transport_basis(),
                    &lease.incarnation,
                )?;
            }
            self.ensure_federation_peer_current_locked(&inner, peer)?;
            if peer.local_role != FederationRole::Satellite
                || binding.central != peer.peer
                || binding.identity.host_id != peer.local.host_id
            {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let record = federation_mailbox_root_locked(&inner, &binding.identity.session_id)?;
            if record.federation_native_creation_id != expected_native_creation_id {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let replacing_automatic = preserve_history && record.federation_mailbox_binding.is_none()
                && record.federation_automatic_mailbox_binding.as_ref().is_some_and(|old|
                    old.central == binding.central && old.identity.host_id == binding.identity.host_id
                        && old.identity.session_id == binding.identity.session_id
                        && binding.placement_generation > old.placement_generation);
            if !replacing_automatic && record.mailbox_binding().is_some_and(|old| old != &binding)
            {
                return Err(ApiError::conflict(
                    "session already has a different domain binding",
                ));
            }
            if record.mailbox_binding() == Some(&binding)
                && (preserve_history || record.federation_mailbox_binding.is_some()) {
                return Ok(None);
            }
            if !preserve_history && record.federation_automatic_mailbox_binding.is_none() {
                refuse_standalone_mailbox_history(&history)?;
            }
            let version = federation_next_generation(inner.federation.version)?;
            let index = inner
                .find_session_index(&binding.identity.session_id)
                .expect("root checked");
            let root = inner.session_mut_by_index(index).expect("root checked");
            if preserve_history {
                if replacing_automatic {
                    let prior = root.federation_automatic_mailbox_binding.clone().expect("replacement checked");
                    if !root.federation_retired_automatic_mailbox_bindings.contains(&prior) {
                        root.federation_retired_automatic_mailbox_bindings.push(prior);
                    }
                }
                root.federation_automatic_mailbox_binding = Some(binding);
            } else {
                root.federation_mailbox_binding = Some(binding);
                root.federation_automatic_mailbox_binding = None;
            }
            inner.federation.version = version;
            inner.federation_durable = false;
            let image = federation_persist_image(&PersistedState::metadata_from_inner(&inner));
            let dispatch = if lease.is_some() {
                self.queue_federation_entrypoint_persistence()?
            } else {
                self.persist_internal_locked_with_dispatch(&inner)
                    .map_err(|_| ApiError::internal("mailbox routing persistence failed"))?
            };
            Ok(Some((image, dispatch)))
        };
        let persistence = match lease {
            Some(lease) => lease.with_incarnation(install)?,
            None => install()?,
        };
        drop(namespace_writer);
        if let Some((image, dispatch)) = persistence {
            let settled = self.finish_federation_durability_with_clock(
                &image,
                dispatch,
                EngramBudgetClock::Real,
            );
            // Only the authenticated command has a pending/recovery contract.
            // An image can change after the binding is installed; a failed
            // fence cannot then prove the original command was refused.
            if lease.is_some() {
                settled.map_err(|_| federation_binding_unknown_outcome())?;
            } else {
                settled?;
            }
        }
        Ok(())
    }
}
