// Explicit central activation. An existing verified report is consumed as
// evidence, while Greg's authorization remains the recorded process rule.

async fn activate_federation_satellite_route(
    AxumPath(id): AxumPath<String>,
    State(app): State<AppState>,
) -> Result<Json<FederationPeerSnapshot>, ApiError> {
    let handle = federation_entrypoint_peer(&app, &id)?;
    let lease = handle
        .channel
        .current_lease()
        .map_err(|_| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
    run_blocking_api(move || {
        let (activated, persistence) = lease
            .with_current(&app, |inner, current| {
                federation_entrypoint_verified(
                    &inner.federation,
                    &inner.preferences.federation,
                    current,
                    &lease.incarnation,
                )?;
                if current.membership == FederationMembershipState::Active {
                    return Ok((current.clone(), None));
                }
                if current.membership != FederationMembershipState::Verified {
                    return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                }
                let mut candidate = inner.federation.clone();
                let peer = candidate.peers.get_mut(&id).expect("current peer checked");
                peer.membership = FederationMembershipState::Active;
                peer.record_generation = federation_next_generation(peer.record_generation)?;
                let mut activated = current.clone();
                activated.membership = peer.membership;
                activated.record_generation = peer.record_generation;
                candidate.version = federation_next_generation(candidate.version)?;
                candidate
                    .validate()
                    .map_err(|_| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
                inner.federation = candidate;
                inner.federation_durable = false;
                let image = federation_persist_image(&PersistedState::metadata_from_inner(inner));
                let dispatch = app.queue_federation_entrypoint_persistence()?;
                Ok((activated, Some((image, dispatch))))
            })
            .map_err(|error| {
                if error.status == StatusCode::CONFLICT {
                    ApiError::conflict(FEDERATION_STALE_AUTHORITY)
                } else {
                    error
                }
            })?;
        if let Some((image, dispatch)) = persistence {
            app.finish_federation_durability_with_clock(&image, dispatch, EngramBudgetClock::Real)?;
        }
        lease.with_current(&app, |inner, current| {
            if *current != activated {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            federation_entrypoint_verified(
                &inner.federation,
                &inner.preferences.federation,
                current,
                &lease.incarnation,
            )?;
            Ok(Json(activated))
        })
    })
    .await
}
