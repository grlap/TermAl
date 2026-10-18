// Owns content-fenced persistence, configuration and explicit continuity reset. Does not own credentials, transport or runtime execution.
// Split from federation_registry.rs while implementing the new domain foundation.

#[cfg_attr(not(test), allow(dead_code))]
impl AppState {
    /// Mutation visibility is conservative until this exact content is durable.
    /// Concurrent changes may supersede a receipt, never silently retarget it.
    fn mutate_federation<T>(
        &self,
        update: impl FnOnce(&mut FederationState, &mut FederationSettings) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.mutate_federation_inner(false, update)
    }

    fn mutate_federation_inner<T>(
        &self,
        reset_continuity: bool,
        update: impl FnOnce(&mut FederationState, &mut FederationSettings) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        self.mutate_federation_with_install_receipt(
            reset_continuity,
            update,
            |_| {},
            EngramBudgetClock::Real,
        )
    }

    // The receipt accounts for this exact in-memory installation even when
    // dispatch or its durability fence fails. It grants no current authority.
    // The callback may only capture the value, never perform IO or take locks.
    fn mutate_federation_with_install_receipt<T>(
        &self,
        reset_continuity: bool,
        update: impl FnOnce(&mut FederationState, &mut FederationSettings) -> Result<T, ApiError>,
        on_install: impl FnOnce(&T),
        clock: EngramBudgetClock,
    ) -> Result<T, ApiError> {
        let (value, image, dispatch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !inner.federation_durable || (inner.federation_continuity_lost && !reset_continuity)
            {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let mut candidate = inner.federation.clone();
            let mut settings = inner.preferences.federation.clone();
            let value = update(&mut candidate, &mut settings)?;
            settings.validate(
                candidate
                    .local
                    .as_ref()
                    .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?,
            )?;
            candidate.version = federation_next_generation(candidate.version)?;
            candidate
                .validate()
                .map_err(|_| ApiError::bad_request("invalid federation transition"))?;
            inner.federation = candidate;
            inner.preferences.federation = settings;
            inner.federation_durable = false;
            on_install(&value);
            let image = federation_persist_image(&PersistedState::metadata_from_inner(&inner));
            let dispatch = self
                .persist_internal_locked_with_dispatch(&inner)
                .map_err(|_| {
                    ApiError::internal("federation persistence failed; admission remains held")
                })?;
            (value, image, dispatch)
        };
        self.finish_federation_durability_with_clock(&image, dispatch, clock)?;
        if reset_continuity {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if federation_persist_image(&PersistedState::metadata_from_inner(&inner)) != image {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            inner.federation_continuity_lost = false;
        }
        Ok(value)
    }

    fn finish_federation_durability_with_clock(
        &self,
        image: &Value,
        dispatch: PersistDispatch,
        clock: EngramBudgetClock,
    ) -> Result<(), ApiError> {
        if matches!(dispatch, PersistDispatch::BackgroundQueued) {
            let deadline = clock.now() + Duration::from_secs(5);
            let (fence, waiter) = PersistFence::new_with_clock(
                PersistFenceTarget::Federation(image.clone()),
                deadline,
                clock,
            );
            self.persist_tx
                .send(PersistRequest::Fence(Box::new(fence)))
                .map_err(|_| {
                    ApiError::internal("federation persistence unavailable; admission remains held")
                })?;
            waiter
                .wait_until(deadline)
                .ok_or_else(|| {
                    ApiError::internal("federation durability unresolved; admission remains held")
                })?
                .map_err(|_| {
                    ApiError::internal("federation durability failed; admission remains held")
                })?;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if federation_persist_image(&PersistedState::metadata_from_inner(&inner)) != *image {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        inner.federation_durable = true;
        Ok(())
    }

    /// An unresolved prior write can be re-confirmed; no new placement or
    /// version is minted while recovering its exact durability disposition.
    fn reconcile_federation_durability(&self) -> Result<(), ApiError> {
        self.reconcile_federation_durability_with_clock(EngramBudgetClock::Real)
    }

    // Tests supply the existing scripted clock only for controlled success.
    // Production recovery always uses the real clock and unchanged deadline.
    fn reconcile_federation_durability_with_clock(
        &self,
        clock: EngramBudgetClock,
    ) -> Result<(), ApiError> {
        let (image, dispatch) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let image = federation_persist_image(&PersistedState::metadata_from_inner(&inner));
            let dispatch = self
                .persist_internal_locked_with_dispatch(&inner)
                .map_err(|_| {
                    ApiError::internal("federation persistence failed; admission remains held")
                })?;
            (image, dispatch)
        };
        self.finish_federation_durability_with_clock(&image, dispatch, clock)
    }

    fn configure_federation(&self, settings: FederationSettings) -> Result<(), ApiError> {
        self.mutate_federation(|domain, current| {
            let local = domain
                .local
                .as_ref()
                .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
            settings.validate(local)?;
            if !domain.mailbox_directory.is_empty()
                && (current.role != settings.role || current.central != settings.central)
            {
                return Err(ApiError::conflict("a participant directory requires explicit authority migration before role replacement"));
            }
            if settings.role == FederationRole::Satellite
                && domain.peers.values().any(|peer| {
                    peer.membership != FederationMembershipState::Revoked
                        && settings.central.as_ref() != Some(&peer.identity)
                })
            {
                return Err(ApiError::conflict(
                    "revoke incompatible peers before changing the central binding",
                ));
            }
            if (current.role != settings.role || current.central != settings.central)
                && domain
                    .placements
                    .values()
                    .any(|row| row.state != FederationPlacementState::Completed)
            {
                return Err(ApiError::conflict(
                    "unresolved placements prevent federation role replacement",
                ));
            }
            if *current != settings {
                domain.links.clear();
                for peer in domain.peers.values_mut() {
                    peer.record_generation = federation_next_generation(peer.record_generation)?;
                    peer.transport_generation =
                        federation_next_generation(peer.transport_generation)?;
                    peer.verification = None;
                    if peer.membership != FederationMembershipState::Revoked {
                        peer.membership = FederationMembershipState::Enrolled;
                    }
                }
            }
            *current = settings;
            Ok(())
        })
    }

    /// Explicit local continuity reset, never an automatic clone takeover.
    /// Existing placements remain retained; every peer must re-enroll.
    fn reset_federation_continuity(
        &self,
        expected: &FederationHostIdentity,
    ) -> Result<(), ApiError> {
        let binding = normalize_local_user_facing_path(
            &fs::canonicalize(self.persistence_path.as_path())
                .map_err(|_| ApiError::internal("federation store is unavailable"))?
                .to_string_lossy(),
        );
        self.mutate_federation_inner(true, |domain, settings| {
            if domain.local.as_ref() != Some(expected) {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let mut local = expected.clone();
            local.host_generation = federation_next_generation(local.host_generation)?;
            domain.local = Some(local.clone());
            domain
                .admission_quarantine
                .get_or_insert(FederationQuarantineReason::DeclaredRestore);
            domain.store_binding = binding;
            domain.links.clear();
            for peer in domain.peers.values_mut() {
                peer.record_generation = federation_next_generation(peer.record_generation)?;
                peer.transport_generation = federation_next_generation(peer.transport_generation)?;
                peer.membership = FederationMembershipState::Revoked;
                peer.verification = None;
            }
            if settings.role == FederationRole::Central {
                settings.central = Some(local);
            }
            Ok(())
        })
    }
}
