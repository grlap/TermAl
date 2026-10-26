// Bounded central discovery with per-peer availability and one admission per
// request. Stable catalog pages skip already-current participants in batches.

impl AppState {
    async fn automatic_discovery_step(&self, cursor: Option<FederationDiscoveryCursor>)
        -> Result<FederationDiscoveryStep, ApiError>
    {
        let position = cursor.unwrap_or_default();
        let (central, following, eligible, unavailable) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let central = federation_mailbox_central_locked(&inner)?;
            let next = match position.host.as_ref() {
                None => inner.federation.peers.first_key_value().map(|(id, _)| id.clone()),
                Some(host) => inner.federation.peers.range((std::ops::Bound::Excluded(host.clone()), std::ops::Bound::Unbounded))
                    .next().map(|(id, _)| id.clone()),
            };
            let peer = position.host.as_ref().and_then(|host| inner.federation.peers.get(host));
            let eligible = position.host.is_none() || peer.is_some_and(|peer|
                peer.membership != FederationMembershipState::Revoked
                    && inner.federation.links.contains_key(&peer.identity.host_id));
            // An enrolled host cannot disappear from alias uniqueness merely
            // because its current authenticated transport is unavailable.
            let unavailable = peer.filter(|peer| !eligible && peer.membership != FederationMembershipState::Revoked)
                .map(|peer| FederationMailboxUnavailableHost { host_id: peer.identity.host_id.clone(),
                    host_name: peer.display_name.clone(), reason: "peer authenticated mailbox link is unavailable".into(), status: 503 });
            (central, next, eligible, unavailable)
        };
        let next_host = following.map(|host| FederationDiscoveryCursor { host: Some(host), after: None });
        if !eligible { return Ok(FederationDiscoveryStep { central, next: next_host, unavailable }); }
        if let Some(host) = position.host.as_ref() {
            let result = self.automatic_remote_catalog_step(&position, central.clone(), next_host.clone()).await;
            return match result {
                Ok(step) => Ok(step),
                Err(error) => {
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    // Local continuity/durability loss is not a peer outage.
                    federation_mailbox_central_locked(&inner)?;
                    let name = inner.federation.peers.get(host).map(|peer| peer.display_name.clone()).unwrap_or_else(|| host.clone());
                    Ok(FederationDiscoveryStep { central, next: next_host,
                        unavailable: Some(FederationMailboxUnavailableHost { host_id: host.clone(), host_name: name,
                            reason: error.message, status: error.status.as_u16() }) })
                }
            };
        }
        let catalog = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            federation_native_catalog_locked(&inner, position.after.as_deref(), central.clone(), central.clone(), String::new())?
        };
        let mut next = catalog.next.map(|after| FederationDiscoveryCursor { host: None, after: Some(after) }).or(next_host);
        for root in catalog.roots {
            // The fast join path is read-only for existing participants. Stop
            // after one new durable admission, preserving the request budget.
            let new = root.binding.is_none();
            let after = root.catalog_after.clone();
            let worker = self.clone();
            let native = root.session_id;
            let result = run_blocking_api(move || worker.join_federation_mailbox_session_with_history(&native, true)).await;
            if let Err(error) = result {
                if error.status.is_server_error() || error.status == StatusCode::LOCKED { return Err(error); }
                eprintln!("local automatic mailbox root refused: {}", error.message);
            }
            if new {
                next = after.map(|after| FederationDiscoveryCursor { host: None, after: Some(after) });
                break;
            }
        }
        Ok(FederationDiscoveryStep { central, next, unavailable: None })
    }

    async fn automatic_remote_catalog_step(&self, position: &FederationDiscoveryCursor,
        central: FederationHostIdentity, next_host: Option<FederationDiscoveryCursor>)
        -> Result<FederationDiscoveryStep, ApiError>
    {
        let host = position.host.as_ref().ok_or_else(federation_stale_link)?;
        let handle = federation_entrypoint_peer(self, host)?;
        let lease = handle.channel.current_lease()?;
        lease.with_current(self, |inner, current| federation_participation_qualified(
            &inner.federation, &inner.preferences.federation, current, &lease.incarnation))?;
        let catalog: FederationParticipationCatalog = handle.rpc.request(FederationRpcRoute::Participation(
            lease.clone(), FederationParticipationCommand::Catalog { after: position.after.clone() },
        )).await?;
        if catalog.host != lease.channel.snapshot.peer || catalog.central != central
            || catalog.link_incarnation != lease.incarnation || catalog.roots.len() > FEDERATION_CATALOG_PAGE
            || catalog.next.is_some() && catalog.next == position.after {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        let mut next = catalog.next.map(|after| FederationDiscoveryCursor { host: position.host.clone(), after: Some(after) }).or(next_host);
        for root in catalog.roots {
            if root.catalog_after.is_none() || root.catalog_after == position.after {
                return Err(ApiError::bad_gateway("peer mailbox catalog omitted its advancing cursor"));
            }
            let current = lease.with_current(self, |inner, peer| {
                federation_participation_qualified(&inner.federation, &inner.preferences.federation, peer, &lease.incarnation)?;
                Ok(root.binding.as_ref().is_some_and(|binding|
                    inner.federation.mailbox_directory.get(&binding.participant_key()).is_some_and(|row|
                        row.binding == *binding && row.name == root.name
                            && federation_mailbox_remote_in_domain(&inner.federation, &inner.preferences.federation, peer, binding))
                    && inner.federation.binding_install_intents.values().any(|intent|
                        intent.binding == *binding && intent.native_creation_id == root.native_creation_id)))
            })?;
            if current { continue; }
            let refused_native = root.session_id.clone();
            let reported_binding = root.binding.clone();
            let after = root.catalog_after.clone();
            if let Err(error) = self.admit_automatic_mailbox_root(&handle, &lease, root).await {
                let current = lease.with_current(self, |inner, current| {
                    federation_participation_qualified(&inner.federation, &inner.preferences.federation, current, &lease.incarnation)?;
                    Ok(current.clone())
                })?;
                if error.status.is_server_error() || error.status == StatusCode::LOCKED { return Err(error); }
                let worker = self.clone();
                let observed = lease.clone();
                let refused_host = host.clone();
                run_blocking_api(move || worker.mutate_federation_core_on_link(Some(&observed), false,
                    |domain, settings, _| {
                        federation_participation_qualified(domain, settings, &current, &observed.incarnation)?;
                        let stale: Vec<_> = domain.mailbox_directory.values().filter(|row|
                            row.binding.identity.host_id == refused_host && row.binding.identity.session_id == refused_native
                                && (reported_binding.as_ref() != Some(&row.binding)
                                    || !federation_mailbox_remote_in_domain(domain, settings, &current, &row.binding)))
                            .map(|row| row.session_id.clone()).collect();
                        for key in stale { domain.mailbox_directory.remove(&key); }
                        Ok(())
                    }, |_| {}, EngramBudgetClock::Real)).await?;
                eprintln!("automatic mailbox root refused: host={host} status={} reason={}", error.status, error.message);
            }
            next = after.map(|after| FederationDiscoveryCursor { host: position.host.clone(), after: Some(after) });
            break;
        }
        Ok(FederationDiscoveryStep { central, next, unavailable: None })
    }
}
