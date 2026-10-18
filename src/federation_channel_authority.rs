// Owns M1 transport-basis validation and exact post-commit link registration.
// Does not own wire identity or SSH. Split from federation_link.rs.

impl FederationChannel {
    fn for_app(
        snapshot: FederationPeerSnapshot,
        capability: FederationPairCapability,
        app: AppState,
    ) -> Self {
        let mut channel = Self::new(snapshot, capability);
        channel.app = Some(app);
        channel
    }

    fn transport_basis(&self) -> FederationPeerSnapshot {
        self.basis
            .read()
            .expect("federation basis lock poisoned")
            .clone()
    }

    fn current_authority(&self) -> Result<FederationPeerSnapshot, ApiError> {
        let (basis, recorded) = {
            let state = self.state.lock().expect("federation link mutex poisoned");
            (self.transport_basis(), state.recorded_incarnation.clone())
        };
        let Some(app) = &self.app else {
            return Ok(basis);
        };
        let inner = app.inner.lock().expect("state mutex poisoned");
        let result = match recorded {
            Some(incarnation) => {
                app.refresh_federation_transport_snapshot_locked(&inner, &basis, &incarnation)
            }
            None => app.federation_reconnect_snapshot_locked(&inner, &basis),
        };
        // Classify from the SAME locked cut used by the authority check. A
        // temporary persistence hold is not a lost remote incarnation.
        result.map_err(|error| {
            if !inner.federation_durable && !inner.federation_continuity_lost {
                ApiError::from_status(
                    StatusCode::LOCKED,
                    "federation persistence is settling; retry",
                )
            } else {
                error
            }
        })
    }

    async fn record_connected(&self, lease: &FederationLinkLease) -> Result<(), ApiError> {
        let expected = self.current_authority()?;
        let channel = self.clone();
        let settlement = self.basis.begin_settlement();
        let incarnation = lease.incarnation.clone();
        // The owned completion survives cancellation of an HTTP handler. If
        // teardown wins while persistence runs, its exact late commit is
        // retired, rather than leaving an unowned live link in M1.
        tokio::spawn(async move {
            // The caller's opening lock may be cancelled. Retain this marker
            // through receipt adoption and exact late-incarnation cleanup;
            // neither supervisor replacement nor worker retry may reset its
            // material fence or backoff during this own transition.
            let _settlement = settlement;
            let (result, installed) = if let Some(app) = &channel.app {
                let app = app.clone();
                let expected = expected.clone();
                let incarnation = incarnation.clone();
                #[cfg(test)]
                let clock = channel.durability_clock.clone();
                #[cfg(test)]
                let published = channel.connection_published.clone();
                tokio::task::spawn_blocking(move || {
                    let mut installed = None;
                    #[cfg(not(test))]
                    let result =
                        app.federation_peer_connected_owned(&expected, incarnation, &mut installed);
                    #[cfg(test)]
                    let result = app.federation_peer_connected_owned_with_clock(
                        &expected,
                        incarnation,
                        &mut installed,
                        clock,
                    );
                    #[cfg(test)]
                    if result.is_ok() {
                        published();
                    }
                    (result, installed)
                })
                .await
                .map_err(|_| federation_transport_unavailable())?
            } else {
                (Ok(expected.clone()), Some(expected.clone()))
            };
            let mut state = channel
                .state
                .lock()
                .expect("federation link mutex poisoned");
            let same_basis = channel.transport_basis().same_transport_as(&expected);
            if same_basis {
                // Adopt only this operation's exact returned post-commit
                // or post-install accounting receipt, never arbitrary current
                // authority. A failed own write must not reset material fencing.
                if let Some(installed) = &installed {
                    *channel
                        .basis
                        .write()
                        .expect("federation basis lock poisoned") = installed.clone();
                }
            }
            let updated = match result {
                Ok(updated) => updated,
                Err(error) => {
                    if let (Some(app), Some(installed)) = (&channel.app, &installed) {
                        app.federation_peer_disconnected(installed, &incarnation);
                    }
                    return Err(error);
                }
            };
            if !same_basis || state.incarnation.as_deref() != Some(incarnation.as_str()) {
                if let Some(app) = &channel.app {
                    app.federation_peer_disconnected(&updated, &incarnation);
                }
                return Err(federation_stale_link());
            }
            state.recorded_incarnation = Some(incarnation);
            Ok(())
        })
        .await
        .map_err(|_| federation_transport_unavailable())?
    }

    // Called only under the link lock, then takes M1's state lock. This
    // immediate runtime-only CAS works while durable bookkeeping is held.
    fn retire_recorded(&self, state: &mut FederationLinkState) {
        if let Some(incarnation) = state.recorded_incarnation.take() {
            if let Some(app) = &self.app {
                app.federation_peer_disconnected(&self.transport_basis(), &incarnation);
            }
        }
    }
}
