// Owns qualified-principal currency checks and ambiguity-aware alias resolution.
// Does not mutate placements, register participants or perform I/O. Split from
// federation_mailbox_directory.rs for the central mailbox protocol.

fn federation_mailbox_central_locked(
    inner: &StateInner,
) -> Result<FederationHostIdentity, ApiError> {
    federation_mailbox_central_checked(inner, true)
}

fn federation_mailbox_central_checked(
    inner: &StateInner, record_refusal: bool,
) -> Result<FederationHostIdentity, ApiError> {
    // Idle worker gates and per-row predicates opt out: they have no refused
    // operation and must not rotate actual refusals out of the bounded history.
    let mut kind = None;
    let unavailable = if !inner.preferences.federation.enabled {
        Some("central mailbox domain is disabled")
    } else if inner.preferences.federation.role != FederationRole::Central {
        Some("this host is not the central mailbox authority")
    } else if !inner.federation_durable {
        kind = Some(ApiErrorKind::FederationMailboxCentralDurability);
        Some("central mailbox authority is not durable")
    } else if inner.federation_continuity_lost {
        Some("central mailbox authority continuity is lost")
    } else if inner.federation.admission_quarantine.is_some() {
        Some("central mailbox authority is quarantined")
    } else if inner.federation.local.as_ref() != inner.preferences.federation.central.as_ref() {
        Some("local host identity differs from the configured central mailbox binding")
    } else if inner.federation.local.is_none() {
        Some("central mailbox host identity is missing")
    } else {
        None
    };
    if let Some(reason) = unavailable {
        if record_refusal {
            inner.federation_durability_diagnostics.refusal(inner.federation_durable,
                if kind == Some(ApiErrorKind::FederationMailboxCentralDurability) {
                    "central_authority_durability"
                } else { "central_authority_check" }, 409);
        }
        let mut error = ApiError::conflict(format!("{FEDERATION_STALE_AUTHORITY}: {reason}"));
        error.kind = kind;
        return Err(error);
    }
    inner
        .federation
        .local
        .clone()
        .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))
}

impl AppState {
    // Diagnostic context only: a later observation cannot establish which
    // authority check failed earlier, and cannot authorize or repair a binding.
    fn federation_mailbox_directory_refusal(&self, session: &str, mut error: ApiError) -> ApiError {
        if error.status != StatusCode::CONFLICT { return error; }
        let inner = self.inner.lock().expect("state mutex poisoned");
        let binding = federation_mailbox_root_locked(&inner, session).ok()
            .and_then(|root| root.mailbox_binding());
        let principal = binding.map(FederationMailboxBinding::participant_key);
        let missing_entry = inner.preferences.federation.role == FederationRole::Central
            && binding.is_some_and(|binding| inner.federation.mailbox_directory
                .get(&binding.participant_key()).is_none_or(|row| row.binding != *binding));
        let central = inner.preferences.federation.central.as_ref()
            .map(|host| format!("{} generation {}", host.host_id, host.host_generation))
            .unwrap_or_else(|| "none".into());
        let observed = if inner.preferences.federation.role == FederationRole::Central {
            // This is later diagnostic context, not another refused operation.
            match federation_mailbox_central_checked(&inner, false) {
                Ok(_) => "central mailbox authority is currently available".to_owned(),
                Err(current) => format!("current central condition: {}", current.message),
            }
        } else {
            "the configured central authority's condition is not observed locally on this host".to_owned()
        };
        let recovery = if error.kind == Some(ApiErrorKind::FederationMailboxCentralDurability) {
            "wait for the existing federation persistence reconciliation to settle, then retry termal_list_sessions with this same session; no host restart or re-enrollment is needed"
        } else if missing_entry {
            "the observed directory entry is missing or differs from this binding; use another current root session on this host for termal_list_sessions; this leaves the original binding unchanged and requires no host restart"
        } else {
            "directory/binding authority changed or is unresolved; have the operator reconcile the observed authority before retrying this same read; do not refresh enrollment or restart solely because of this refusal"
        };
        error.message = format!(
            "{}; observed context: caller {session}, mailbox binding {}, configured central {central}; {observed}; recovery: {recovery}",
            error.message, principal.as_deref().unwrap_or("none"),
        );
        error
    }
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
    if !federation_mailbox_central_checked(inner, false)
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
    let basis = federation_mailbox_alias_basis_locked(inner);
    let rows: Vec<_> = inner
        .federation
        .mailbox_directory
        .values()
        .filter_map(|row| current_federation_mailbox_participant_locked(inner, row))
        .collect();
    select_federation_mailbox_alias(&rows, reference, Some(&basis), federation_mailbox_alias_row)
        .cloned()
}
