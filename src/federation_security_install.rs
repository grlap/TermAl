// Owns cancellation-independent durable context installation and exact teardown.
// Does not perform network IO or grant admission. New A1/M1 ownership seam.

struct FederationControlGuard {
    channel: FederationChannel,
    owner: u64,
    incarnation: String,
    cancelled: Arc<AtomicBool>,
}

impl Drop for FederationControlGuard {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.channel.fence_security(self.owner, &self.incarnation);
    }
}

impl FederationChannel {
    fn next_security_owner(&self) -> Result<u64, ApiError> {
        self.current_authority()?;
        let mut state = self.state.lock().expect("federation link mutex poisoned");
        state.next_owner = state
            .next_owner
            .checked_add(1)
            .ok_or_else(federation_auth_refused)?;
        let owner = state.next_owner;
        // Publish the attempt while owner allocation is still serialized, so
        // a delayed older begin cannot overwrite its successor observation.
        self.begin_attestation(owner);
        drop(state);
        Ok(owner)
    }

    async fn install_security(
        &self,
        context: Arc<FederationSecurityContext>,
    ) -> Result<FederationControlGuard, ApiError> {
        let guard = FederationControlGuard {
            channel: self.clone(),
            owner: context.owner,
            incarnation: context.incarnation.clone(),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let opening = self.opening.clone().lock_owned().await;
        let expected = self.current_authority()?;
        if self
            .state
            .lock()
            .expect("federation link mutex poisoned")
            .installed_owner
            >= context.owner
        {
            return Err(federation_auth_refused());
        }
        let settlement = self.basis.begin_settlement();
        let channel = self.clone();
        let cancelled = guard.cancelled.clone();
        tokio::spawn(async move {
            let _settlement = settlement;
            let _opening = opening;
            let (result, installed) = if let Some(app) = &channel.app {
                let app = app.clone();
                let expected = expected.clone();
                let incarnation = context.incarnation.clone();
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
                if let Some(receipt) = &installed {
                    *channel
                        .basis
                        .write()
                        .expect("federation basis lock poisoned") = receipt.clone();
                    // This authenticated mutation already withdrew the prior
                    // runtime UUID in M1. Never leave its stale local lease.
                    state.recorded_incarnation = None;
                    state.incarnation = None;
                    if let Some(old) = state.security.take() {
                        old.retire();
                    }
                }
            }
            let cleanup = |receipt: &FederationPeerSnapshot| {
                if let Some(app) = &channel.app {
                    app.federation_peer_disconnected(receipt, &context.incarnation);
                }
            };
            let receipt = match result {
                Ok(receipt) => receipt,
                Err(error) => {
                    if let Some(receipt) = &installed {
                        cleanup(receipt);
                    }
                    return Err(error);
                }
            };
            if !same_basis
                || cancelled.load(Ordering::SeqCst)
                || context.owner <= state.installed_owner
            {
                cleanup(&receipt);
                return Err(federation_auth_refused());
            }
            state.installed_owner = context.owner;
            state.incarnation = Some(context.incarnation.clone());
            state.recorded_incarnation = Some(context.incarnation.clone());
            state.security = Some(context.clone());
            let _ = context.installed_at.set(channel.control.clock.now());
            drop(state);
            channel.observe_attestation(Some(context.owner));
            Ok(())
        })
        .await
        .map_err(|_| federation_transport_unavailable())??;
        Ok(guard)
    }
}
