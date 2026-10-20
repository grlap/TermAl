// Owns content-fenced persistence, configuration and explicit continuity reset. Does not own credentials, transport or runtime execution.
// Split from federation_registry.rs while implementing the new domain foundation.

/// Who is configuring federation: the local operator route, whose satellite
/// changes must name the version they were read at, or a domain caller.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FederationSettingsFence {
    Operator,
    Domain,
}

/// Facts about this host that a federation mutation may guard on, read under
/// the same state lock as the candidate it installs.
struct FederationMutationContext {
    /// The central of each local session's domain mailbox binding.
    bound_centrals: Vec<FederationHostIdentity>,
}

impl FederationMutationContext {
    fn locked(inner: &StateInner) -> Self {
        Self {
            bound_centrals: inner
                .sessions
                .iter()
                .filter_map(|record| record.federation_mailbox_binding.as_ref())
                .map(|binding| binding.central.clone())
                .collect(),
        }
    }

    fn bound_to(&self, central: &FederationHostIdentity) -> usize {
        self.bound_centrals.iter().filter(|bound| *bound == central).count()
    }
}

/// Applies `settings` to the candidate domain under the mutation's lock, in
/// order: the version fence; the role and central validation; a stale central
/// (older than the bound one or a retained peer record of it); a satellite's
/// sessions bound to its current central; the participant directory; peers
/// enrolled under another central; unresolved placements. A change
/// invalidates every link and verification.
fn apply_federation_settings(
    domain: &mut FederationState,
    current: &mut FederationSettings,
    context: &FederationMutationContext,
    settings: FederationSettings,
    expected_version: Option<u64>,
    fence: FederationSettingsFence,
) -> Result<(), ApiError> {
    let satellite_change =
        current.role == FederationRole::Satellite || settings.role == FederationRole::Satellite;
    match expected_version {
        None if fence == FederationSettingsFence::Operator && satellite_change => {
            return Err(ApiError::bad_request(
                "a satellite configuration names the expectedVersion it was read at",
            )
            .with_kind(ApiErrorKind::FederationExpectedVersionRequired));
        }
        Some(version) if version != domain.version => {
            return Err(ApiError::conflict(format!(
                "federation settings changed since version {version} (now {}); read them again",
                domain.version
            ))
            .with_kind(ApiErrorKind::FederationVersionConflict));
        }
        _ => {}
    }
    let local = domain
        .local
        .as_ref()
        .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
    settings
        .validate(local)
        .map_err(|error| error.with_kind(ApiErrorKind::FederationInvalidBinding))?;
    let replaces = current.role != settings.role || current.central != settings.central;
    if let (FederationRole::Satellite, Some(next)) = (settings.role, settings.central.as_ref()) {
        // Stale against the highest generation of that central this host
        // knows: the current binding, or any retained peer record of it,
        // revoked included.
        let known = [
            current
                .central
                .as_ref()
                .filter(|bound| current.role == FederationRole::Satellite && bound.host_id == next.host_id)
                .map(|bound| bound.host_generation),
            domain
                .peers
                .get(&next.host_id)
                .map(|peer| peer.identity.host_generation),
        ]
        .into_iter()
        .flatten()
        .max();
        if let Some(known) = known.filter(|known| next.host_generation < *known) {
            return Err(ApiError::conflict(format!(
                "central identity generation {} is older than generation {known} this host already knows",
                next.host_generation
            ))
            .with_kind(ApiErrorKind::FederationCentralStale));
        }
    }
    if let (FederationRole::Satellite, Some(bound)) = (current.role, current.central.as_ref()) {
        let sessions = context.bound_to(bound);
        if replaces && sessions > 0 {
            return Err(ApiError::conflict(format!(
                "{sessions} local session(s) hold a domain mailbox binding to the current central; \
                 delete or retire them before replacing the role or the central (no unbind \
                 workflow exists yet)"
            ))
            .with_kind(ApiErrorKind::FederationBoundSessions));
        }
    }
    if !domain.mailbox_directory.is_empty() && replaces {
        return Err(ApiError::conflict(
            "a participant directory requires explicit authority migration before role replacement",
        ));
    }
    if settings.role == FederationRole::Satellite
        && domain.peers.values().any(|peer| {
            peer.membership != FederationMembershipState::Revoked
                && settings.central.as_ref() != Some(&peer.identity)
        })
    {
        return Err(
            ApiError::conflict("revoke incompatible peers before changing the central binding")
                .with_kind(ApiErrorKind::FederationCentralMismatch),
        );
    }
    if replaces
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
            peer.transport_generation = federation_next_generation(peer.transport_generation)?;
            peer.verification = None;
            if peer.membership != FederationMembershipState::Revoked {
                peer.membership = FederationMembershipState::Enrolled;
            }
        }
    }
    *current = settings;
    Ok(())
}

/// Local sessions holding a domain mailbox binding to `central`.
fn federation_bound_session_count(inner: &StateInner, central: &FederationHostIdentity) -> usize {
    inner
        .sessions
        .iter()
        .filter_map(|record| record.federation_mailbox_binding.as_ref())
        .filter(|binding| &binding.central == central)
        .count()
}

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
        self.mutate_federation_core(
            reset_continuity,
            |domain, settings, _| update(domain, settings),
            on_install,
            clock,
        )
    }

    // The one federation mutation path. `update` also sees facts about this
    // host read under the same state lock as the candidate, so a guard on
    // them cannot race a concurrent change.
    fn mutate_federation_core<T>(
        &self,
        reset_continuity: bool,
        update: impl FnOnce(
            &mut FederationState,
            &mut FederationSettings,
            &FederationMutationContext,
        ) -> Result<T, ApiError>,
        on_install: impl FnOnce(&T),
        clock: EngramBudgetClock,
    ) -> Result<T, ApiError> {
        self.mutate_federation_core_on_link(None, reset_continuity, update, on_install, clock)
    }

    // The lease is held only while installing the candidate, never while
    // waiting for durability. Preserve the established link -> state order.
    fn mutate_federation_core_on_link<T>(
        &self,
        lease: Option<&FederationLinkLease>,
        reset_continuity: bool,
        update: impl FnOnce(
            &mut FederationState,
            &mut FederationSettings,
            &FederationMutationContext,
        ) -> Result<T, ApiError>,
        on_install: impl FnOnce(&T),
        clock: EngramBudgetClock,
    ) -> Result<T, ApiError> {
        let install = || {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(lease) = lease {
                self.refresh_federation_transport_snapshot_locked(
                    &inner,
                    &lease.channel.transport_basis(),
                    &lease.incarnation,
                )?;
            }
            if !inner.federation_durable || (inner.federation_continuity_lost && !reset_continuity)
            {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let context = FederationMutationContext::locked(&inner);
            let mut candidate = inner.federation.clone();
            let mut settings = inner.preferences.federation.clone();
            let value = update(&mut candidate, &mut settings, &context)?;
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
            let dispatch = if lease.is_some() {
                self.queue_federation_entrypoint_persistence()?
            } else {
                self.persist_internal_locked_with_dispatch(&inner)
                    .map_err(|_| {
                        ApiError::internal("federation persistence failed; admission remains held")
                    })?
            };
            Ok((value, image, dispatch))
        };
        let (value, image, dispatch) = match lease {
            Some(lease) => lease.with_incarnation(install)?,
            None => install()?,
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
        // Metadata may commit while a hot session is deferred by the bounded
        // collector. Recovery ticks use this same composite target, so they
        // cannot clear an INSTALL hold before the exact native row commits.
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if federation_persist_image(&PersistedState::metadata_from_inner(&inner)) != *image {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let sessions = inner
                .sessions
                .iter()
                .filter(|row| !row.hidden)
                .filter_map(|row| {
                    row.federation_mailbox_binding.as_ref().map(|binding| {
                        FederationNativeBindingFence {
                            binding: binding.clone(),
                            native_creation_id: row.federation_native_creation_id.clone(),
                        }
                    })
                })
                .collect::<Vec<_>>();
            if sessions.is_empty() {
                PersistFenceTarget::Federation(image.clone())
            } else {
                PersistFenceTarget::FederationBindings(Box::new(FederationBindingsFence {
                    image: image.clone(),
                    sessions,
                }))
            }
        };
        if matches!(dispatch, PersistDispatch::BackgroundQueued) {
            let deadline = clock.now() + Duration::from_secs(5);
            let (fence, waiter) = PersistFence::new_with_clock(target, deadline, clock);
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
        } else if matches!(&target, PersistFenceTarget::FederationBindings(_)) {
            // The legacy synchronous fallback has already committed. Keep its
            // row proof exact as well; never mark a quarantined row durable.
            let connection = rusqlite::Connection::open(self.persistence_path.as_path())
                .map_err(|_| ApiError::internal("binding durability readback unavailable"))?;
            if !target
                .is_already_durable(&connection)
                .map_err(|_| ApiError::internal("binding durability readback failed"))?
            {
                return Err(ApiError::internal(
                    "binding durability unresolved; admission remains held",
                ));
            }
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
        self.configure_federation_fenced(settings, None, FederationSettingsFence::Domain)
    }

    /// The domain configuration path, as the operator route and domain
    /// callers share it. For the operator, a change from or to the satellite
    /// role is fenced on the domain version the operator read; any change
    /// that names a version is fenced on it.
    fn configure_federation_fenced(
        &self,
        settings: FederationSettings,
        expected_version: Option<u64>,
        fence: FederationSettingsFence,
    ) -> Result<(), ApiError> {
        self.mutate_federation_core(
            false,
            |domain, current, context| {
                apply_federation_settings(domain, current, context, settings, expected_version, fence)
            },
            |_| {},
            EngramBudgetClock::Real,
        )
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
