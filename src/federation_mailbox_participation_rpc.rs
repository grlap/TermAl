// Pair-authenticated bounded discovery and routing-only installation.
// Each request owns at most one admission; no nested full-domain sweep.

async fn serve_federation_participation(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(command): Json<FederationParticipationCommand>,
) -> Result<Json<Value>, ApiError> {
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let app = lease.channel.app.clone().ok_or_else(federation_transport_unavailable)?;
    let permit = peer.mailbox_permit.ok_or_else(federation_transport_unavailable)?;
    match command {
        FederationParticipationCommand::Catalog { after } => {
            run_blocking_api(move || {
                let _permit = permit;
                lease.with_current(&app, |inner, current| {
                    if current.local_role != FederationRole::Satellite || inner.federation.admission_quarantine.is_some() {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    let catalog = federation_native_catalog_locked(inner, after.as_deref(), current.local.clone(),
                        current.central.clone(), lease.incarnation.clone())?;
                    serde_json::to_value(catalog).map(Json).map_err(|_| ApiError::internal("mailbox catalog encoding failed"))
                })
            }).await
        }
        FederationParticipationCommand::Describe { native_session_id } => {
            run_blocking_api(move || {
                let _permit = permit;
                lease.with_current(&app, |inner, current| {
                    if current.local_role != FederationRole::Satellite || inner.federation.admission_quarantine.is_some() {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    let root = federation_mailbox_root_locked(inner, &native_session_id)?;
                    serde_json::to_value(FederationParticipationCatalog { host: current.local.clone(), central: current.central.clone(),
                        link_incarnation: lease.incarnation.clone(), roots: vec![federation_catalog_root(root)], next: None,
                    }).map(Json).map_err(|_| ApiError::internal("native root encoding failed"))
                })
            }).await
        }
        FederationParticipationCommand::Discover { cursor } => {
            lease.with_current(&app, |inner, current| federation_participation_qualified(
                &inner.federation, &inner.preferences.federation, current, &lease.incarnation))?;
            let step = app.automatic_discovery_step(cursor).await?;
            lease.with_current(&app, |inner, current| {
                federation_participation_qualified(&inner.federation, &inner.preferences.federation, current, &lease.incarnation)?;
                serde_json::to_value(step).map(Json).map_err(|_| ApiError::internal("discovery step encoding failed"))
            })
        }
        FederationParticipationCommand::Ensure { native_session_id, native_creation_id, directory: _ } => {
            let host = lease.with_current(&app, |inner, current| {
                federation_participation_qualified(&inner.federation, &inner.preferences.federation, current, &lease.incarnation)?;
                Ok(current.peer.host_id.clone())
            })?;
            let handle = federation_entrypoint_peer(&app, &host)?;
            let described: FederationParticipationCatalog = handle.rpc.request(FederationRpcRoute::Participation(
                lease.clone(), FederationParticipationCommand::Describe { native_session_id: native_session_id.clone() },
            )).await?;
            if described.host != lease.channel.snapshot.peer || described.central != lease.channel.snapshot.central
                || described.link_incarnation != lease.incarnation || described.roots.len() != 1 || described.next.is_some() {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let root = described.roots.into_iter().next().filter(|root|
                root.session_id == native_session_id && root.native_creation_id == native_creation_id)
                .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
            let participant = app.admit_automatic_mailbox_root(&handle, &lease, root).await?;
            serde_json::to_value(participant).map(Json).map_err(|_| ApiError::internal("mailbox participant encoding failed"))
        }
        FederationParticipationCommand::Install { binding, native_creation_id } => {
            if lease.channel.snapshot.local_role != FederationRole::Satellite {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            federation_validate_binding_install(&binding, &native_creation_id, "automatic-mailbox-install")?;
            run_blocking_api(move || {
                let _permit = permit;
                let current = lease.with_current(&app, |_, current| Ok(current.clone()))?;
                app.install_federation_mailbox_binding_preserving_history(&current, binding.clone(), &native_creation_id, Some(&lease), true)?;
                lease.with_current(&app, |inner, _| {
                    let root = federation_mailbox_root_locked(inner, &binding.identity.session_id)?;
                    if root.federation_native_creation_id != native_creation_id || root.mailbox_binding() != Some(&binding) {
                        return Err(federation_binding_unknown_outcome());
                    }
                    serde_json::to_value(FederationMailboxActor { binding,
                        name: federation_mailbox_display_name(&root.session.name) }).map(Json)
                        .map_err(|_| ApiError::internal("mailbox actor encoding failed"))
                })
            }).await
        }
    }
}
