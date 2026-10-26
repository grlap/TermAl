// Owns automatic native-root discovery and mailbox-only binding admission.
// Does not activate peers, reserve executor capacity, or move mailbox history.

fn federation_participation_qualified(
    domain: &FederationState, settings: &FederationSettings,
    current: &FederationPeerSnapshot, incarnation: &str,
) -> Result<(), ApiError> {
    let qualified = settings.enabled && settings.role == FederationRole::Central
        && domain.admission_quarantine.is_none() && domain.local.as_ref() == Some(&current.local)
        && settings.central.as_ref() == Some(&current.central)
        && domain.links.get(&current.peer.host_id).map(String::as_str) == Some(incarnation)
        && domain.peers.get(&current.peer.host_id).is_some_and(|peer|
            peer.record_generation == current.record_generation && peer.identity == current.peer
                && peer.enrollment_generation == current.enrollment_generation
                && matches!(peer.membership, FederationMembershipState::Verified | FederationMembershipState::Active | FederationMembershipState::Draining)
                && peer.verified_on(domain.links.get(&current.peer.host_id)));
    if qualified { Ok(()) } else { Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY)) }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
enum FederationParticipationCommand {
    Catalog { after: Option<String> },
    Describe { native_session_id: String },
    Discover { cursor: Option<FederationDiscoveryCursor> },
    Ensure { native_session_id: String, native_creation_id: String, directory: bool },
    Install { binding: FederationMailboxBinding, native_creation_id: String },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationParticipationRoot {
    session_id: String,
    native_creation_id: String,
    binding: Option<FederationMailboxBinding>,
    name: String,
    automatic: bool,
    #[serde(default)]
    catalog_after: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationParticipationCatalog {
    host: FederationHostIdentity,
    central: FederationHostIdentity,
    link_incarnation: String,
    roots: Vec<FederationParticipationRoot>,
    next: Option<String>,
}

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationDiscoveryCursor {
    host: Option<String>,
    after: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationDiscoveryStep {
    central: FederationHostIdentity,
    next: Option<FederationDiscoveryCursor>,
    unavailable: Option<FederationMailboxUnavailableHost>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationMailboxUnavailableHost {
    host_id: String,
    host_name: String,
    reason: String,
    status: u16,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationMailboxDirectorySnapshot {
    participants: Vec<FederationMailboxParticipant>,
    unavailable_hosts: Vec<FederationMailboxUnavailableHost>,
}

fn federation_directory_alias_available(reference: &str, unavailable: &[FederationMailboxUnavailableHost]) -> bool {
    if reference.starts_with("domain-session-") || reference.starts_with("session-") { return true; }
    let host = reference.split_once("::").map(|(host, _)| host);
    unavailable.iter().all(|row| !row.host_id.is_empty() && host.is_some_and(|host|
        !host.eq_ignore_ascii_case(&row.host_id) && !host.eq_ignore_ascii_case(&row.host_name)))
}

fn federation_directory_incomplete() -> FederationMailboxUnavailableHost {
    FederationMailboxUnavailableHost { host_id: String::new(), host_name: "Directory".into(),
        reason: "additional mailbox participants remain undiscovered; read the directory again".into(), status: 503 }
}

impl AppState {
    // Native ids remain local-host locators. They never select a same-named
    // session on another host or fall through to a display alias.
    fn automatic_local_mailbox_target(&self, target: &str) -> Result<String, ApiError> {
        if !target.starts_with("session-") { return Ok(target.to_owned()); }
        let inner = self.inner.lock().expect("state mutex poisoned");
        let root = federation_mailbox_root_locked(&inner, target)?;
        root.mailbox_binding().map(|binding| binding.participant_key())
            .ok_or_else(|| ApiError::conflict("local native mailbox participant is not qualified"))
    }

    fn automatic_mailbox_role(&self, session: &str) -> Result<FederationRole, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let role = if inner.preferences.federation.enabled {
            inner.preferences.federation.role.clone()
        } else { FederationRole::Standalone };
        if role != FederationRole::Standalone && federation_mailbox_root_locked(&inner, session).is_err() {
            return Ok(FederationRole::Standalone);
        }
        Ok(role)
    }

    async fn ensure_automatic_mailbox_participation(&self, session: &str, directory: bool)
        -> Result<Vec<FederationMailboxUnavailableHost>, ApiError>
    {
        match self.automatic_mailbox_role(session)? {
            FederationRole::Standalone => Ok(Vec::new()),
            FederationRole::Central => {
                let worker = self.clone();
                let native = session.to_owned();
                run_blocking_api(move || worker.join_federation_mailbox_session_with_history(&native, true)).await?;
                if directory { self.discover_automatic_mailbox_roots().await }
                else { Ok(Vec::new()) }
            }
            FederationRole::Satellite => {
                let (central, creation, bound, automatic) = {
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    let root = federation_mailbox_root_locked(&inner, session)?;
                    (inner.preferences.federation.central.clone().ok_or_else(federation_stale_link)?,
                     root.federation_native_creation_id.clone(), root.mailbox_binding().is_some(),
                     root.federation_automatic_mailbox_binding.is_some())
                };
                if bound && !automatic && !directory { return Ok(Vec::new()); }
                let handle = self.federation_peer_handle(&central.host_id).ok_or_else(federation_transport_unavailable)?;
                let lease = handle.channel.current_lease().map_err(|_| federation_mailbox_unavailable())?;
                if !bound || automatic {
                    let participant: FederationMailboxParticipant = handle.rpc.request(FederationRpcRoute::Participation(
                        lease.clone(), FederationParticipationCommand::Ensure {
                            native_session_id: session.to_owned(), native_creation_id: creation.clone(), directory: false,
                        },
                    )).await?;
                    lease.with_current(self, |inner, current| {
                        let root = federation_mailbox_root_locked(inner, session)?;
                        if root.federation_native_creation_id != creation
                            || participant.binding.central != current.central
                            || participant.binding.identity.host_id != current.local.host_id
                            || participant.binding.identity.session_id != session
                            || root.mailbox_binding() != Some(&participant.binding) {
                            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                        }
                        Ok(())
                    })?;
                }
                let mut unavailable = Vec::new();
                if directory {
                    let mut cursor = None;
                    let mut finished = false;
                    for _ in 0..32768 {
                        let step: FederationDiscoveryStep = handle.rpc.request(FederationRpcRoute::Participation(
                            lease.clone(), FederationParticipationCommand::Discover { cursor: cursor.clone() },
                        )).await?;
                        if step.central != central || step.next.is_some() && step.next == cursor {
                            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                        }
                        if let Some(error) = step.unavailable { unavailable.push(error); }
                        cursor = step.next;
                        if cursor.is_none() { finished = true; break; }
                    }
                    if !finished { unavailable.push(federation_directory_incomplete()); }
                    lease.with_current(self, |inner, _| {
                        let root = federation_mailbox_root_locked(inner, session)?;
                        if root.federation_native_creation_id != creation || root.mailbox_binding().is_none() {
                            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                        }
                        Ok(())
                    })?;
                }
                Ok(unavailable)
            }
        }
    }

    async fn discover_automatic_mailbox_roots(&self) -> Result<Vec<FederationMailboxUnavailableHost>, ApiError> {
        let mut cursor = None;
        let mut unavailable = Vec::new();
        for _ in 0..32768 {
            let step = self.automatic_discovery_step(cursor.clone()).await?;
            if step.next.is_some() && step.next == cursor {
                return Err(ApiError::conflict("mailbox discovery cursor did not advance"));
            }
            if let Some(error) = step.unavailable { unavailable.push(error); }
            cursor = step.next;
            if cursor.is_none() { return Ok(unavailable); }
        }
        unavailable.push(federation_directory_incomplete());
        Ok(unavailable)
    }

    async fn mailbox_directory_snapshot(&self, session: &str) -> Result<FederationMailboxDirectorySnapshot, ApiError> {
        let unavailable_hosts = self.ensure_automatic_mailbox_participation(session, true).await?;
        let mut participants: Vec<FederationMailboxParticipant> = self.dispatch_federation_mailbox_operation(
            session, FederationMailboxOperation::Directory).await?;
        participants.retain(|row| !unavailable_hosts.iter().any(|host| host.host_id == row.binding.identity.host_id));
        Ok(FederationMailboxDirectorySnapshot { participants, unavailable_hosts })
    }

    async fn admit_automatic_mailbox_root(
        &self, handle: &FederationPeerHandle, lease: &FederationLinkLease, mut root: FederationParticipationRoot,
    ) -> Result<FederationMailboxParticipant, ApiError> {
        let current = lease.with_current(self, |inner, current| {
            federation_participation_qualified(&inner.federation, &inner.preferences.federation, current, &lease.incarnation)?;
            if !(matches!(current.membership, FederationMembershipState::Verified | FederationMembershipState::Active)
                || current.membership == FederationMembershipState::Draining && root.binding.is_some()) {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            Ok(current.clone())
        })?;
        if let Some(binding) = root.binding.as_ref() {
            binding.validate()?;
            if binding.identity.session_id != root.session_id {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            // An existing binding is never replaced or promoted to a new principal.
            let valid = lease.with_current(self, |inner, peer| {
                if !federation_mailbox_remote_in_domain(&inner.federation, &inner.preferences.federation, peer, binding) {
                    return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                }
                for placement in inner.federation.placements.values().filter(|placement| placement.identity == binding.identity) {
                    if inner.federation.binding_install_intents.get(&placement.admission_key)
                        .is_some_and(|intent| intent.native_creation_id != root.native_creation_id) {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                }
                Ok(())
            });
            if let Err(error) = valid {
                if !root.automatic || binding.central != current.central || binding.identity.host_id != current.peer.host_id {
                    return Err(error);
                }
                // Only an automatic routing record may be superseded. An
                // explicit execution binding is never adopted or replaced.
                root.binding = None;
            }
        }
        if root.binding.is_none() {
            let worker = self.clone();
            let observed = lease.clone();
            let peer = current.clone();
            let native = root.session_id.clone();
            let creation = root.native_creation_id.clone();
            let has_stale = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner.federation.placements.values().any(|placement| placement.mailbox_only
                    && placement.identity.host_id == peer.peer.host_id && placement.identity.session_id == native
                    && placement.state != FederationPlacementState::Completed
                    && (!federation_mailbox_placement_authority(&inner.federation, &inner.preferences.federation, placement)
                        || placement.peer.enrollment_generation != peer.enrollment_generation
                        || inner.federation.binding_install_intents.get(&placement.admission_key)
                            .is_some_and(|intent| intent.native_creation_id != creation)))
            };
            if has_stale {
                run_blocking_api(move || worker.mutate_federation_core_on_link(Some(&observed), false,
                    |domain, settings, _| {
                        federation_participation_qualified(domain, settings, &peer, &observed.incarnation)?;
                        let retired: Vec<_> = domain.placements.values().filter(|placement| placement.mailbox_only
                            && placement.identity.host_id == peer.peer.host_id && placement.identity.session_id == native
                            && placement.state != FederationPlacementState::Completed
                            && (!federation_mailbox_placement_authority(domain, settings, placement)
                                || placement.peer.enrollment_generation != peer.enrollment_generation
                                || domain.binding_install_intents.get(&placement.admission_key)
                                    .is_some_and(|intent| intent.native_creation_id != creation)))
                            .map(|placement| placement.identity.clone()).collect();
                        for placement in domain.placements.values_mut().filter(|placement| retired.contains(&placement.identity)) {
                            placement.state = FederationPlacementState::Completed;
                        }
                        domain.mailbox_directory.retain(|_, row| !retired.contains(&row.binding.identity));
                        Ok(())
                    }, |_| {}, EngramBudgetClock::Real)).await?;
            }
        }
        if let Some(binding) = root.binding.as_ref() {
            let existing = lease.with_current(self, |inner, _| Ok(inner.federation.mailbox_directory
                .get(&binding.participant_key()).filter(|row| row.binding == *binding && row.name == root.name).cloned()))?;
            if let Some(participant) = existing { return Ok(participant); }
            let worker = self.clone();
            let observed = lease.clone();
            let actor = FederationMailboxActor { binding: binding.clone(), name: root.name.clone() };
            return run_blocking_api(move || {
                let peer = observed.with_current(&worker, |_, peer| Ok(peer.clone()))?;
                worker.register_federation_mailbox_actor(&peer, &actor)
            }).await;
        }
        // A committed reservation can outlive an interrupted INSTALL. Its
        // migrated authority, rather than the new transport's admission key,
        // decides whether the same native root can finish that installation.
        let retained = lease.with_current(self, |inner, peer| {
            federation_participation_qualified(&inner.federation, &inner.preferences.federation, peer, &lease.incarnation)?;
            for placement in inner.federation.placements.values().filter(|placement|
                placement.mailbox_only && placement.state != FederationPlacementState::Completed
                    && placement.identity.host_id == peer.peer.host_id
                    && placement.identity.session_id == root.session_id
                    && federation_mailbox_placement_authority(&inner.federation, &inner.preferences.federation, placement)) {
                let Some(intent) = inner.federation.binding_install_intents.get(&placement.admission_key) else { continue; };
                if intent.native_creation_id == root.native_creation_id
                    && intent.binding.central == peer.central
                    && intent.binding.identity == placement.identity
                    && intent.binding.placement_generation == placement.placement_generation
                    && intent.peer_enrollment_generation == peer.enrollment_generation
                    && intent.configuration_basis == peer.configuration_basis()
                    && intent.execution_basis == placement.execution_basis {
                    intent.validate()?;
                    return Ok(Some(intent.binding.clone()));
                }
            }
            Ok(None)
        })?;
        let binding = if let Some(binding) = retained { binding } else {
            let key = format!("mailbox-auto-{}", sha256_hex(&serde_json::to_vec(&(
                &current.central, &current.peer, current.enrollment_generation, current.transport_generation,
                current.record_generation, &lease.incarnation, &root.session_id, &root.native_creation_id,
            )).expect("native authority serializes")));
            let worker = self.clone();
            let observed = lease.clone();
            let native = root.session_id.clone();
            let creation = root.native_creation_id.clone();
            run_blocking_api(move || worker.reserve_federation_placement_kind(
                &current, key.clone(), native, 0, 0, Some(&observed), true,
                |domain, settings, placement| {
                    federation_participation_qualified(domain, settings, &current, &observed.incarnation)?;
                    if !federation_mailbox_placement_authority(domain, settings, placement)
                        || !placement.mailbox_only || placement.state == FederationPlacementState::Completed {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    let binding = FederationMailboxBinding { central: current.central.clone(),
                        identity: placement.identity.clone(), placement_generation: placement.placement_generation };
                    let intent = FederationBindingInstallIntent {
                        operation_key: key.clone(), native_creation_id: creation, binding: binding.clone(),
                        admission_key: key.clone(), execution_basis: placement.execution_basis.clone(),
                        peer_enrollment_generation: current.enrollment_generation,
                        peer_verification_basis: placement.execution_basis.clone(), configuration_basis: current.configuration_basis(),
                    };
                    intent.validate()?;
                    if domain.binding_install_intents.get(&key).is_some_and(|prior| prior != &intent) {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    domain.binding_install_intents.insert(key, intent);
                    Ok(binding)
                },
            )).await?
        };
        lease.channel.control.observe("mailboxParticipationReserved", 0);
        let actor: FederationMailboxActor = handle.rpc.request(FederationRpcRoute::Participation(
            lease.clone(), FederationParticipationCommand::Install { binding: binding.clone(), native_creation_id: root.native_creation_id },
        )).await?;
        if actor.binding != binding { return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY)); }
        let worker = self.clone();
        let observed = lease.clone();
        let participant = run_blocking_api(move || {
            let peer = observed.with_current(&worker, |_, peer| Ok(peer.clone()))?;
            worker.register_federation_mailbox_actor(&peer, &actor)
        }).await?;
        lease.with_current(self, |inner, peer| {
            if participant.binding != binding || participant.session_id != binding.participant_key()
                || !federation_mailbox_remote_in_domain(&inner.federation, &inner.preferences.federation, peer, &binding)
                || inner.federation.mailbox_directory.get(&participant.session_id) != Some(&participant) {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            Ok(participant)
        })
    }
}
