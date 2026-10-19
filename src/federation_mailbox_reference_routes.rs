// Local reference provisioning, content-fenced publication and fresh resolve.
// No native installation, mailbox append or executor routing. Offline replay
// returns only an exact previously published descriptor, never use authority.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProvisionFederationReferenceRequest {
    participant_id: String,
    idempotency_key: String,
}

async fn list_federation_mailbox_references(
    AxumPath(owner): AxumPath<String>, State(app): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let inner = app.inner.lock().expect("state mutex poisoned");
    let root = federation_mailbox_root_locked(&inner, &owner)?;
    // Listing committed descriptors is deliberately not an authority check.
    let rows: Vec<_> = inner.federation.mailbox_references.values()
        .filter(|row| row.owner == owner
            && row.owner_creation == root.federation_native_creation_id && row.is_published())
        .map(FederationMailboxReference::descriptor).collect();
    Ok(Json(json!({ "references": rows })))
}

async fn provision_federation_mailbox_reference(
    AxumPath(owner): AxumPath<String>, State(app): State<AppState>,
    Json(request): Json<ProvisionFederationReferenceRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    validate_federation_reference(&request.idempotency_key)
        .map_err(|_| ApiError::bad_request("invalid mailbox reference idempotency key"))?;
    if request.participant_id.len() > 1024 || !request.participant_id.starts_with("domain-session-") {
        return Err(ApiError::bad_request("an exact qualified remote participant is required"));
    }
    let intent = sha256_hex(request.idempotency_key.as_bytes());
    let prior = {
        let inner = app.inner.lock().expect("state mutex poisoned");
        let root = federation_mailbox_root_locked(&inner, &owner)?;
        inner.federation.mailbox_references.values().find(|row| {
            row.owner == owner && row.owner_creation == root.federation_native_creation_id
                && row.intent == intent
        }).cloned()
    };
    if let Some(row) = &prior {
        if row.participant_id != request.participant_id {
            return Err(ApiError::conflict("reference key conflicts with its original target"));
        }
        let state = app.clone();
        let prepared = row.clone();
        if let Some(descriptor) = run_blocking_api(move || {
            state.reconcile_federation_reference_publication(&prepared)
        }).await? {
            return Ok((StatusCode::CREATED, Json(json!({ "reference": descriptor }))));
        }
        if !row.is_published() {
            // An exact failed/uncertain own write is reconciled, not replaced
            // with a successor or a second handle. It stays hidden meanwhile.
            let state = app.clone();
            #[cfg(test)]
            let clock = app.federation_peer_handle(&row.binding.identity.host_id)
                .map(|handle| handle.channel.durability_clock.clone()).unwrap_or_default();
            #[cfg(not(test))]
            let clock = EngramBudgetClock::Real;
            run_blocking_api(move || state.reconcile_federation_durability_with_clock(clock)).await?;
        }
    }
    let context = app.capture_federation_reference(&owner, &request.participant_id)?;
    if prior.as_ref().is_some_and(|row| !context.matches(row)) {
        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
    }
    let proof = context.proof(&app).await?;
    if prior.as_ref().is_some_and(|row| row.native_creation_id != proof.native_creation_id) {
        return Err(ApiError::conflict("native creation changed; old reference intent is retained"));
    }
    let state = app.clone();
    let descriptor = run_blocking_api(move || {
        let (row, persistence) = context.with_current(&state, |inner| {
            // Recheck under the commit-adjacent lock, not only before I/O.
            if let Some(existing) = inner.federation.mailbox_references.values().find(|row| {
                row.owner == context.owner && row.owner_creation == context.owner_creation
                    && row.intent == intent
            }).cloned() {
                if !context.matches(&existing) || existing.native_creation_id != proof.native_creation_id {
                    return Err(ApiError::conflict("reference intent changed"));
                }
                return Ok((existing, None));
            }
            let row = FederationMailboxReference {
                id: format!("mailbox-reference-{}", Uuid::new_v4()),
                generation: federation_next_generation(inner.federation.version)?,
                participant_id: context.binding.participant_key(),
                native_creation_id: proof.native_creation_id.clone(),
                binding: context.binding.clone(), owner: context.owner.clone(),
                owner_creation: context.owner_creation.clone(), owner_binding: context.owner_binding.clone(),
                intent: intent.clone(), peer: context.peer.clone(), placement: context.placement.clone(),
                link_incarnation: context.lease.incarnation.clone(), publication: None,
            };
            row.validate()?;
            if inner.federation.mailbox_references.contains_key(&row.id) {
                return Err(ApiError::conflict("reference handle collision"));
            }
            inner.federation.mailbox_references.insert(row.id.clone(), row.clone());
            inner.federation.version = row.generation;
            inner.federation_durable = false;
            let image = federation_persist_image(&PersistedState::metadata_from_inner(inner));
            let dispatch = state.persist_internal_locked_with_dispatch(inner)
                .map_err(|_| ApiError::internal("reference persistence failed; publication remains held"))?;
            Ok((row, Some((image, dispatch))))
        })?;
        if let Some((image, dispatch)) = persistence {
            state.finish_federation_durability_with_clock(&image, dispatch, context.durability_clock())?;
        }
        state.publish_federation_reference(&context, &row)
    }).await?;
    Ok((StatusCode::CREATED, Json(json!({ "reference": descriptor }))))
}

async fn resolve_federation_mailbox_reference(
    AxumPath((owner, id)): AxumPath<(String, String)>, State(app): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    if !mailbox_reference_handle(&id).is_ok_and(|handle| handle.as_deref() == Some(id.as_str())) {
        return Err(ApiError::not_found("canonical mailbox reference required"));
    }
    let row = {
        let inner = app.inner.lock().expect("state mutex poisoned");
        let root = federation_mailbox_root_locked(&inner, &owner)?;
        inner.federation.mailbox_references.get(&id).filter(|row| {
            row.is_published() && row.owner == owner && row.owner_creation == root.federation_native_creation_id
        }).cloned().ok_or_else(|| ApiError::not_found("committed mailbox reference not found"))?
    };
    let context = app.capture_federation_reference(&owner, &row.participant_id)?;
    if !context.matches(&row) {
        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
    }
    let proof = context.proof(&app).await?;
    if proof.native_creation_id != row.native_creation_id {
        return Err(ApiError::conflict("native creation changed"));
    }
    context.with_current(&app, |inner| {
        if inner.federation.mailbox_references.get(&id) != Some(&row) {
            return Err(ApiError::conflict("mailbox reference changed"));
        }
        Ok(Json(row.descriptor()))
    })
}
