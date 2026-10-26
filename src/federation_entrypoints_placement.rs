// Central-only explicit placement establishment and recovery. A retained key
// recovers its immutable install intent before considering a new PREPARE.

async fn reserve_federation_mailbox_placement_route(
    AxumPath(id): AxumPath<String>,
    State(app): State<AppState>,
    Json(request): Json<FederationPlacementRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    validate_federation_reference(&request.admission_key)?;
    let handle = federation_entrypoint_peer(&app, &id)?;
    let lease = handle
        .channel
        .current_lease()
        .map_err(|_| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
    let (current, prior, automatic) = lease
        .with_current(&app, |inner, current| {
            federation_entrypoint_verified(
                &inner.federation,
                &inner.preferences.federation,
                current,
                &lease.incarnation,
            )?;
            if current.membership != FederationMembershipState::Active {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            let prior = inner
                .federation
                .binding_install_intents
                .get(&request.admission_key)
                .cloned();
            if inner
                .federation
                .placements
                .contains_key(&request.admission_key)
                && prior.is_none()
            {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            if let Some(intent) = &prior {
                if intent.binding.identity.host_id != id
                    || intent.binding.identity.session_id != request.native_session_id
                {
                    return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                }
                let placement = inner
                    .federation
                    .placements
                    .get(&request.admission_key)
                    .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
                intent.ensure_authority(&inner.federation, current, placement)?;
            }
            let automatic = inner.federation.placements.values().find(|placement|
                placement.mailbox_only && placement.identity.host_id == id
                    && placement.identity.session_id == request.native_session_id
                    && placement.state != FederationPlacementState::Completed
                    && federation_mailbox_placement_authority(&inner.federation, &inner.preferences.federation, placement)
            ).map(|placement| {
                let intent = inner.federation.binding_install_intents.get(&placement.admission_key)
                    .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
                intent.ensure_authority(&inner.federation, current, placement)?;
                Ok::<_, ApiError>(intent.clone())
            }).transpose()?;
            Ok((current.clone(), prior, automatic))
        })
        .map_err(|error| {
            if error.status == StatusCode::CONFLICT {
                ApiError::conflict(FEDERATION_STALE_AUTHORITY)
            } else {
                error
            }
        })?;
    let native_creation_id = if let Some(intent) = &prior {
        intent.native_creation_id.clone()
    } else if let Some(intent) = &automatic {
        // The explicit Active-only reservation upgrades routing authority in
        // place. INSTALL still proves the original native creation remotely.
        intent.native_creation_id.clone()
    } else {
        let prepared: FederationBindingPreparation = handle
            .rpc
            .request(FederationRpcRoute::Binding(
                lease.clone(),
                FederationBindingCommand::Prepare {
                    native_session_id: request.native_session_id.clone(),
                },
            ))
            .await?;
        if prepared.host != current.peer
            || prepared.central != current.central
            || prepared.native_session_id != request.native_session_id
            || prepared.link_incarnation != lease.incarnation
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        prepared.native_creation_id
    };
    let worker = app.clone();
    let observed = lease.clone();
    let (placement, intent) = run_blocking_api(move || {
        worker.reserve_federation_placement_with(
            &current,
            request.admission_key.clone(),
            request.native_session_id.clone(),
            0,
            0,
            Some(&observed),
            |domain, settings, placement| {
                // The helper checks the current link under the mutation lock.
                // Revalidate the actual report, not only a cached checklist.
                federation_entrypoint_verified(domain, settings, &current, &observed.incarnation)?;
                if current.membership != FederationMembershipState::Active {
                    return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                }
                let peer = domain
                    .peers
                    .get(&id)
                    .ok_or_else(|| ApiError::conflict(FEDERATION_STALE_AUTHORITY))?;
                if placement.peer.local != current.local
                    || placement.peer.central != current.central
                    || placement.peer.peer != current.peer
                    || placement.peer.enrollment_generation != current.enrollment_generation
                    || placement.peer.endpoint != current.endpoint
                    || placement.peer.pair_capability_ref != current.pair_capability_ref
                    || placement.execution_basis != peer.verification_basis()
                    || placement.state == FederationPlacementState::Completed
                {
                    return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                }
                let binding = FederationMailboxBinding {
                    central: current.central.clone(),
                    identity: placement.identity.clone(),
                    placement_generation: placement.placement_generation,
                };
                let intent = if let Some(existing) =
                    domain.binding_install_intents.get(&request.admission_key)
                {
                    if existing.binding != binding
                        || existing.native_creation_id != native_creation_id
                    {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    existing.ensure_authority(domain, &current, placement)?;
                    existing.clone()
                } else {
                    if prior.is_some() {
                        return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                    }
                    FederationBindingInstallIntent {
                        operation_key: Uuid::new_v4().to_string(),
                        native_creation_id,
                        binding,
                        admission_key: request.admission_key.clone(),
                        execution_basis: placement.execution_basis.clone(),
                        peer_enrollment_generation: current.enrollment_generation,
                        peer_verification_basis: peer.verification_basis(),
                        configuration_basis: current.configuration_basis(),
                    }
                };
                intent.validate()?;
                let mut placement = placement.clone();
                placement.peer = current.clone();
                placement.link_incarnation = observed.incarnation.clone();
                domain
                    .placements
                    .insert(request.admission_key.clone(), placement.clone());
                domain
                    .binding_install_intents
                    .insert(request.admission_key.clone(), intent.clone());
                Ok((placement, intent))
            },
        )
    })
    .await?;
    lease.with_current(&app, |inner, current| {
        federation_entrypoint_verified(
            &inner.federation,
            &inner.preferences.federation,
            current,
            &lease.incarnation,
        )?;
        if current.membership != FederationMembershipState::Active
            || inner.federation.placements.get(&placement.admission_key) != Some(&placement)
            || inner
                .federation
                .binding_install_intents
                .get(&placement.admission_key)
                != Some(&intent)
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(())
    })?;
    let installed = handle
        .rpc
        .request::<FederationBindingInstalled>(FederationRpcRoute::Binding(
            lease.clone(),
            FederationBindingCommand::Install {
                binding: intent.binding.clone(),
                expected_native_creation_id: intent.native_creation_id.clone(),
                operation_key: intent.operation_key.clone(),
            },
        ))
        .await;
    match installed {
        Ok(installed) => {
            if installed.operation_key != intent.operation_key
                || installed.native_creation_id != intent.native_creation_id
                || installed.binding != intent.binding
                || installed.participant.binding != intent.binding
                || installed.participant.session_id != intent.binding.participant_key()
            {
                return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
            }
            lease.with_current(&app,|inner,current| {
                federation_entrypoint_verified(&inner.federation,&inner.preferences.federation,current,&lease.incarnation)?;
                if current.membership!=FederationMembershipState::Active
                    || inner.federation.placements.get(&placement.admission_key)!=Some(&placement)
                    || inner.federation.binding_install_intents.get(&placement.admission_key)!=Some(&intent)
                    || inner.federation.mailbox_directory.get(&installed.participant.session_id)!=Some(&installed.participant) {
                    return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
                }
                Ok((StatusCode::OK,Json(json!({"state":"installed","operationKey":intent.operation_key,
                    "nativeCreationId":intent.native_creation_id,"placement":placement,"participant":installed.participant}))))
            })
        }
        Err(error)
            if error.status.is_server_error()
                || matches!(
                    error.status,
                    StatusCode::LOCKED | StatusCode::REQUEST_TIMEOUT
                ) =>
        {
            // Admission stays reserved. The remote install may already be
            // durable; only the original key can reconcile its outcome.
            Ok((
                StatusCode::ACCEPTED,
                Json(
                    json!({"state":"pending","operationKey":intent.operation_key,
                "nativeCreationId":intent.native_creation_id,"placement":placement}),
                ),
            ))
        }
        Err(error) => Err(error),
    }
}
