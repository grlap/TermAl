// Owns qualified-principal currency checks and ambiguity-aware alias resolution.
// Does not mutate placements, register participants or perform I/O. Split from
// federation_mailbox_directory.rs for the central mailbox protocol.

fn federation_mailbox_central_locked(
    inner: &StateInner,
) -> Result<FederationHostIdentity, ApiError> {
    if !inner.preferences.federation.enabled
        || inner.preferences.federation.role != FederationRole::Central
        || !inner.federation_durable
        || inner.federation_continuity_lost
        || inner.federation.admission_quarantine.is_some()
        || inner.federation.local.as_ref() != inner.preferences.federation.central.as_ref()
    {
        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
    }
    inner
        .federation
        .local
        .clone()
        .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))
}

fn federation_mailbox_remote_in_domain(
    domain: &FederationState,
    settings: &FederationSettings,
    peer: &FederationPeerSnapshot,
    binding: &FederationMailboxBinding,
) -> bool {
    let current = domain.peers.get(&peer.peer.host_id);
    settings.enabled
        && settings.role == FederationRole::Central
        && domain.local.as_ref() == Some(&binding.central)
        && binding.central == peer.central
        && peer.local == binding.central
        && binding.identity.host_id == peer.peer.host_id
        && current.is_some_and(|row| {
            row.record_generation == peer.record_generation
                && row.transport_generation == peer.transport_generation
                && row.identity == peer.peer
                && row.enrollment_generation == peer.enrollment_generation
                && row.endpoint == peer.endpoint
                && row.pair_capability_ref == peer.pair_capability_ref
        })
        && domain.placements.values().any(|placement| {
            placement.identity == binding.identity
                && placement.placement_generation == binding.placement_generation
                && placement.state != FederationPlacementState::Completed
                && federation_mailbox_placement_authority(domain, settings, placement)
        })
}

fn federation_mailbox_participant_current_locked(
    inner: &StateInner,
    participant: &FederationMailboxParticipant,
) -> bool {
    if !federation_mailbox_central_locked(inner)
        .is_ok_and(|central| central == participant.binding.central)
    {
        return false;
    }
    if participant.binding.identity.host_id == participant.binding.central.host_id {
        return federation_mailbox_root_locked(inner, &participant.binding.identity.session_id)
            .is_ok_and(|record| {
                record.mailbox_binding() == Some(&participant.binding)
            });
    }
    federation_snapshot_locked(inner, &participant.binding.identity.host_id).is_ok_and(|peer| {
        federation_mailbox_remote_in_domain(
            &inner.federation,
            &inner.preferences.federation,
            &peer,
            &participant.binding,
        )
    })
}

fn current_federation_mailbox_participant_locked(
    inner: &StateInner,
    participant: &FederationMailboxParticipant,
) -> Option<FederationMailboxParticipant> {
    if !federation_mailbox_participant_current_locked(inner, participant) {
        return None;
    }
    let mut participant = participant.clone();
    if participant.binding.identity.host_id == participant.binding.central.host_id {
        let record =
            federation_mailbox_root_locked(inner, &participant.binding.identity.session_id).ok()?;
        participant.name = federation_mailbox_display_name(&record.session.name);
    }
    Some(participant)
}

fn resolve_federation_mailbox_participant_locked(
    inner: &StateInner,
    reference: &str,
) -> Result<FederationMailboxParticipant, ApiError> {
    // Typed reference handles are resolved only by the reference facade, never
    // reinterpreted as display aliases (which are case-insensitive).
    if !matches!(mailbox_reference_handle(reference), Ok(None)) {
        return Err(ApiError::not_found("mailbox reference requires explicit resolution"));
    }
    if let Some(exact) = inner.federation.mailbox_directory.get(reference) {
        return current_federation_mailbox_participant_locked(inner, exact)
            .ok_or_else(|| ApiError::not_found("qualified mailbox participant not found"));
    }
    // Canonical ids never fall back to display aliases, including stale ids.
    if reference.starts_with("domain-session-") || reference.starts_with("session-") {
        return Err(ApiError::not_found(
            "qualified id required; use the directory sessionId or exact Host::Name",
        ));
    }
    let mut matches = inner
        .federation
        .mailbox_directory
        .values()
        .filter_map(|row| current_federation_mailbox_participant_locked(inner, row))
        .filter(|row| {
            row.name.eq_ignore_ascii_case(reference)
                || format!("{}::{}", row.host_name, row.name).eq_ignore_ascii_case(reference)
                || format!("{}::{}", row.binding.identity.host_id, row.name)
                    .eq_ignore_ascii_case(reference)
        });
    let first = matches
        .next()
        .ok_or_else(|| ApiError::not_found("qualified mailbox participant not found"))?;
    if matches.next().is_some() {
        return Err(ApiError::bad_request(
            "mailbox participant alias is ambiguous; use its exact qualified sessionId",
        ));
    }
    Ok(first)
}
