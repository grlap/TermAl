// Owns the pure per-session containment plan from a locked state snapshot.
// Does not publish settings, send requests, interrupt providers or lift exclusions.

struct EngramContainmentPrepared {
    obligation: EngramSessionObligation,
    target: EngramBindingTarget,
    can_checkpoint: bool,
    read_only_terminal: bool,
    runtime_running: bool,
    consume_admission_retry: bool,
    consume_bind_retry: bool,
}

struct EngramContainmentAuthority {
    route: EngramAuthorityStoreKey,
    confirmed: bool,
}

// Local routing validation does not launch readiness, open the authority store,
// recreate a control connection, or turn an unavailable remote into a veto.
fn validate_engram_containment_authority(
    project: &Project,
    settings: &EngramProjectSettings,
) -> Result<EngramContainmentAuthority, ApiError> {
    validate_engram_project_home_for_update(settings)?;
    let (_, marker, home) = validate_engram_project_connection_paths(project, settings)?;
    resolve_engram_containment_authority(settings, &marker, &home)
}

// An existing bound connection can close its grant without re-enabling its
// executable. Both callers retain the same declaration/store identity checks.
fn resolve_engram_containment_authority(
    settings: &EngramProjectSettings,
    marker: &FsPath,
    home: &FsPath,
) -> Result<EngramContainmentAuthority, ApiError> {
    let declaration = read_engram_diagnostic_declaration(marker)?;
    let original_path = work_database_path(home, declaration.trim());
    let authority = if settings.authority_store_key.is_none()
        && !original_path.try_exists().map_err(|_| {
            ApiError::conflict(
                "containment_authority_unavailable: original store path cannot be inspected",
            )
        })? {
        // Legacy admitted settings may predate the cached diagnostic key and
        // even the first materialized database. Resolve their original route
        // from the existing canonical home/declaration, without opening or
        // creating a store or claiming any fact about a producer grant.
        let database_path = canonicalize_path_with_existing_ancestor(&original_path)?;
        if read_engram_diagnostic_declaration(marker)? != declaration {
            return Err(ApiError::conflict(
                "containment_authority_changed: original project declaration changed",
            ));
        }
        EngramAuthorityStoreKey {
            database_path,
            project_id: declaration.trim().to_owned(),
        }
    } else {
        let database = fs::canonicalize(&original_path).map_err(|_| {
            ApiError::conflict("containment_authority_unavailable: original Engram store identity cannot be resolved")
        })?;
        validate_engram_diagnostic_identity(declaration.trim(), &database, marker, home)?
    };
    if settings
        .authority_store_key
        .as_ref()
        .is_some_and(|original| original != &authority)
    {
        return Err(ApiError::conflict(
            "containment_authority_changed: original Engram store identity no longer matches",
        ));
    }
    Ok(EngramContainmentAuthority {
        route: authority,
        confirmed: settings.authority_store_key.is_some(),
    })
}

fn prepare_engram_containment_entry_locked(
    inner: &StateInner,
    session_id: &str,
    project_id: &str,
    authority: &EngramAuthorityStoreKey,
    authority_confirmed: bool,
) -> Result<Option<EngramContainmentPrepared>, ApiError> {
    if engram_session_obligation_locked(inner, session_id)
        .is_some_and(|entry| entry.kind != EngramSessionObligationKind::ExistingCheckpoint)
    {
        return Ok(None);
    }
    let index = inner
        .find_session_index(session_id)
        .ok_or_else(|| ApiError::conflict("containment session disappeared"))?;
    let record = &inner.sessions[index];
    // A terminal acknowledged historical mirror is not the next grant's
    // checkpoint component and must not lend its operation id to a new duty.
    let existing_checkpoint = record
        .engram
        .checkpoint_operation
        .as_ref()
        .filter(|operation| {
            record.engram.checkpoint_in_progress
                || engram_obligation_requires_exclusion(inner, &operation.protection)
        });
    let unresolved = record.engram.active_grant_id.is_some()
        || record.engram.uncertain_grant_id.is_some()
        || record.engram.pending_dispatch.is_some()
        || record.engram.bind_in_progress
        || record.engram.checkpoint_in_progress
        || (!record.engram.begins_recorded && record.engram.routing_token.is_some())
        || record
            .queued_prompts
            .iter()
            .any(QueuedPromptRecord::has_engram_intent);
    if !unresolved {
        return Ok(None);
    }
    let target = if let Some(mirror) = existing_checkpoint {
        let target = EngramCheckpointRecoveryView::for_off_adoption(inner, session_id);
        if target.is_none()
            && selected_checkpoint_operation(&inner.engram_session_obligations, mirror).is_some()
            && engram_checkpoint_debt_can_outlive_owner(inner, &mirror.protection)
            && inner.engram_session_obligations.all_protections().filter(|entry| {
                engram_obligation_requires_exclusion(inner, entry)
                    && engram_obligation_matches_session(inner, entry, session_id)
            }).all(|entry| engram_checkpoint_debt_can_outlive_owner(inner, entry))
        {
            // The original report already has durable protection. Runtime or
            // settings drift prevents adoption, not the operator's OFF switch.
            // Retain its exact ledger and mirror without creating a new target.
            return Ok(None);
        }
        target
    } else {
        project_engram_binding_target_locked(inner, session_id).map_err(|_| {
            ApiError::conflict("original Engram containment connection cannot be reconstructed")
        })?
    }
    .ok_or_else(|| ApiError::conflict("original Engram containment connection is unavailable"))?;
    let owner_generation = record
        .engram
        .dispatch_generation
        .checked_add(1)
        .ok_or_else(|| ApiError::conflict("Engram session ownership generation is exhausted"))?;
    let runtime_running = matches!(
        record.session.status,
        SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
    ) || record.runtime_stop_in_progress;
    // Use the scheduler's full strict match on the actual predecessor, before
    // containment moves the generation. An old record is not a current owner.
    let consume_admission_retry = authority_confirmed
        && !runtime_running
        && !record.engram.project_reset_in_progress
        && !record.dedicated_cleanup_holds_admission()
        && !engram_retry_attempt_in_flight(record)
        && record.engram.abort_retry.is_none()
        && record.engram.bind_retry.is_none()
        && record.engram.active_grant_id.is_none()
        && record.engram.uncertain_grant_id.is_none()
        && !record.engram.bind_in_progress
        && !record.engram.checkpoint_in_progress
        && record.queued_prompts.front().is_some_and(|head| {
            (head.engram_bind.is_some() != head.engram_evaluate.is_some())
                && head.engram_evaluate.as_ref().is_none_or(|evaluate| {
                    evaluate.prepared_begin.is_none() && evaluate.begun_grant_id.is_none()
                })
        })
        && record.engram.admission_retry.as_ref().is_some_and(|retry| {
            retry.authority == engram_abort_authority(&target)
                && engram_admission_retry_head_matches(record, retry)
        });
    let consume_bind_retry = authority_confirmed
        && !runtime_running
        && !record.engram.project_reset_in_progress
        && !record.dedicated_cleanup_holds_admission()
        && !engram_retry_attempt_in_flight(record)
        && record.engram.abort_retry.is_none()
        && record.engram.admission_retry.is_none()
        && record.engram.active_grant_id.is_none()
        && record.engram.uncertain_grant_id.is_none()
        && record.engram.disabled_reason.is_none()
        && !record.engram.operator_paused
        && !record.engram.bind_in_progress
        && !record.engram.checkpoint_in_progress
        && record
            .engram
            .bind_retry_runtime
            .as_ref()
            .is_some_and(|runtime| record.runtime.matches_runtime_token(runtime))
        && record.queued_prompts.front().is_some_and(|head| {
            head.engram_bind.is_some()
                && head.engram_evaluate.is_none()
                && head.engram_waiting
                && !head.engram_interrupted
        })
        && record.engram.bind_retry.as_ref().is_some_and(|retry| {
            matches!(retry.proof.phase, EngramBindRetryPhase::Prepared { .. })
                && retry.proof.authority == engram_abort_authority(&target)
                && engram_bind_retry_head_matches(record, &retry.proof)
        });
    let can_checkpoint = authority_confirmed
        && existing_checkpoint.is_none()
        && !runtime_running
        && record.engram.begins_recorded
        && !record.engram.bind_in_progress
        && !record.engram.checkpoint_in_progress
        && record.engram.pending_dispatch.is_none()
        && !record
            .queued_prompts
            .iter()
            .any(QueuedPromptRecord::has_engram_intent)
        && record.engram.uncertain_grant_id.is_none()
        && record.engram.routing_token.is_some()
        && record.engram.active_grant_id.is_some();
    let read_only_terminal = authority_confirmed
        && !runtime_running
        && record.engram.begins_recorded
        && !record.engram.bind_in_progress
        && !record.engram.checkpoint_in_progress
        && record.engram.pending_dispatch.is_none()
        && !record
            .queued_prompts
            .iter()
            .any(QueuedPromptRecord::has_engram_intent)
        && record.engram.active_grant_id.is_none()
        && record
            .engram
            .uncertain_grant_id
            .as_deref()
            .is_some_and(|grant| grant != ENGRAM_UNCERTAIN_GRANT_UNKNOWN)
        && record.engram.routing_token.is_some();
    let (basis, frozen_request) =
        if can_checkpoint {
            (
                EngramSessionObligationBasis::CheckpointUnsettled,
                serde_json::to_value(target.project_reset_checkpoint_request().map_err(|_| {
                    ApiError::conflict("original checkpoint request is unavailable")
                })?)
                .map_err(|_| ApiError::internal("failed encoding containment checkpoint"))?,
            )
        } else {
            let image = EngramUnsettledControlImage {
                operation: "unsettled_control_image".to_owned(),
                routing_token: record.engram.routing_token.clone(),
                active_grant_id: record.engram.active_grant_id.clone(),
                uncertain_grant_id: record.engram.uncertain_grant_id.clone(),
                begins_recorded: record.engram.begins_recorded,
                bind_in_progress: record.engram.bind_in_progress,
                checkpoint_in_progress: record.engram.checkpoint_in_progress,
                existing_checkpoint: existing_checkpoint.cloned(),
                runtime_running,
                parked_admission_retry: consume_admission_retry
                    .then(|| record.engram.admission_retry.clone())
                    .flatten(),
                parked_bind_retry: consume_bind_retry
                    .then(|| record.engram.bind_retry.clone())
                    .flatten(),
                queued_prompts: record.queued_prompts.iter().cloned().collect(),
                begin_requested: record
                    .engram
                    .pending_dispatch
                    .as_ref()
                    .and_then(|pending| pending.begin_requested.clone()),
                dispatch_generation: record.engram.dispatch_generation,
            };
            (
                EngramSessionObligationBasis::ControlStateUnsettled,
                serde_json::to_value(image)
                    .map_err(|_| ApiError::internal("failed encoding containment recovery"))?,
            )
        };
    let obligation = EngramSessionObligation {
        id: existing_checkpoint
            .map(|operation| operation.original.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        project_id: project_id.to_owned(),
        authority: authority.clone(),
        authority_confirmed,
        connection: target.connection.clone(),
        external_ref: target.external_ref.clone(),
        local_session_id: session_id.to_owned(),
        owner_generation,
        prompt_id: record
            .queued_prompts
            .front()
            .map(|queued| queued.pending_prompt.id.clone()),
        routing_token: target.routing_token.clone().unwrap_or_default(),
        grant_id: target
            .active_grant_id
            .clone()
            .or(target.uncertain_grant_id.clone())
            .unwrap_or_default(),
        kind: EngramSessionObligationKind::CheckpointContainment,
        // Sent is write-ahead: a crash after this acknowledgement
        // cannot prove that the ensuing transport call was unsent.
        phase: if can_checkpoint {
            EngramSessionObligationPhase::Sent
        } else {
            EngramSessionObligationPhase::Intended
        },
        basis,
        reason_code: if !authority_confirmed {
            "original_authority_unconfirmed"
        } else if runtime_running {
            "provider_interruption_unconfirmed"
        } else {
            "control_settlement_unconfirmed"
        }
        .to_owned(),
        frozen_request,
        settlement_evidence: Value::Null,
    };
    Ok(Some(EngramContainmentPrepared {
        obligation,
        target,
        can_checkpoint,
        read_only_terminal,
        runtime_running,
        consume_admission_retry,
        consume_bind_retry,
    }))
}
