// Pair-authenticated satellite commands. Only INSTALL mutates the native root;
// PREPARE and the existing native-binding proof cannot create an association.

async fn serve_federation_binding_command(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(command): Json<FederationBindingCommand>,
) -> Result<Json<Value>, ApiError> {
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let app = lease
        .channel
        .app
        .clone()
        .ok_or_else(federation_transport_unavailable)?;
    if lease.channel.snapshot.local_role != FederationRole::Satellite {
        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
    }
    let permit = peer.mailbox_permit;
    match command {
        FederationBindingCommand::Prepare { native_session_id } => {
            run_blocking_api(move || {
                let _permit = permit;
                let _writer = app
                    .mailbox_store
                    .lock_writer("preparing a native mailbox binding")
                    .map_err(mailbox_api_error)?;
                let history = app
                    .mailbox_store
                    .standalone_mailbox_history(&native_session_id)
                    .map_err(mailbox_api_error)?;
                refuse_standalone_mailbox_history(&history)?;
                lease.with_current(&app, |inner, current| {
                    if inner.federation.admission_quarantine.is_some() {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    let root = federation_mailbox_root_locked(inner, &native_session_id)?;
                    if root.federation_mailbox_binding.is_some() {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    serde_json::to_value(FederationBindingPreparation {
                        host: current.local.clone(),
                        central: current.central.clone(),
                        native_session_id,
                        native_creation_id: root.federation_native_creation_id.clone(),
                        link_incarnation: lease.incarnation.clone(),
                    })
                    .map(Json)
                    .map_err(|_| ApiError::internal("binding preparation encoding failed"))
                })
            })
            .await
        }
        FederationBindingCommand::Install {
            binding,
            expected_native_creation_id,
            operation_key,
        } => {
            federation_validate_binding_install(
                &binding,
                &expected_native_creation_id,
                &operation_key,
            )?;
            let worker = app.clone();
            let observed = lease.clone();
            let expected = binding.clone();
            let creation = expected_native_creation_id.clone();
            run_blocking_api(move || {
                let _permit = permit;
                let current = observed.with_current(&worker, |_, current| Ok(current.clone()))?;
                worker.install_federation_mailbox_binding_on_link(
                    &current,
                    expected.clone(),
                    &creation,
                    Some(&observed),
                )?;
                observed.channel.control.observe("bindingInstalled", 0);
                let postcheck = observed.with_current(&worker, |inner, _| {
                    let root =
                        federation_mailbox_root_locked(inner, &expected.identity.session_id)?;
                    if root.federation_mailbox_binding.as_ref() != Some(&expected)
                        || root.federation_native_creation_id != creation
                    {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    Ok(())
                });
                observed
                    .channel
                    .control
                    .observe("bindingPostcheck", u64::from(postcheck.is_err()));
                postcheck.map_err(|_| federation_binding_unknown_outcome())
            })
            .await?;
            let participant: FederationMailboxParticipant = app
                .federation_mailbox_operation(
                    &binding.identity.session_id,
                    FederationMailboxOperation::Register,
                )
                .await
                .map_err(|_| federation_binding_unknown_outcome())?;
            lease
                .with_current(&app, |inner, _| {
                    let root = federation_mailbox_root_locked(inner, &binding.identity.session_id)?;
                    if participant.binding != binding
                        || participant.session_id != binding.participant_key()
                        || root.federation_native_creation_id != expected_native_creation_id
                        || root.federation_mailbox_binding.as_ref() != Some(&binding)
                    {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    serde_json::to_value(FederationBindingInstalled {
                        operation_key,
                        native_creation_id: expected_native_creation_id,
                        binding,
                        participant,
                    })
                    .map(Json)
                    .map_err(|_| ApiError::internal("binding result encoding failed"))
                })
                .map_err(|_| federation_binding_unknown_outcome())
        }
    }
}
