// Owns current-successor boot validation and durable transfer to the ordinary
// begun-grant lifecycle. Does not replay RPCs or infer provider delivery.

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramContainmentAdmissionProgress {
    operation: String,
    prompt_id: String,
    intent_fingerprint: String,
    grant_id: String,
    begin_key: String,
}

// Repeated OFF may retain the current Begin/grant duty after admission has
// already transferred to it. Shared keys are legal only for this typed exact
// successor relationship, never for a newly invented admission cycle.
fn containment_admission_retained_duty_successor(
    history: &EngramSessionObligation,
    successor: &EngramSessionObligation,
) -> bool {
    if history.id == successor.id
        || history.phase != EngramSessionObligationPhase::Confirmed
        || !containment_admission_transfer_matches(history)
        || successor.kind != EngramSessionObligationKind::CheckpointContainment
        || successor.basis != EngramSessionObligationBasis::ControlStateUnsettled
        || history.authority != successor.authority
        || history.connection != successor.connection
        || history.local_session_id != successor.local_session_id
        || history.external_ref != successor.external_ref
    {
        return false;
    }
    let Ok(transfer) = serde_json::from_value::<EngramContainmentAdmissionTransfer>(
        history.settlement_evidence.clone(),
    ) else {
        return false;
    };
    let Ok(image) =
        serde_json::from_value::<EngramUnsettledControlImage>(successor.frozen_request.clone())
    else {
        return false;
    };
    let Some(head) = image.queued_prompts.first() else {
        return false;
    };
    let Some(evaluate) = &head.engram_evaluate else {
        return false;
    };
    let Some(prepared) = &transfer.evaluate.prepared_begin else {
        return false;
    };
    let EngramControlRequest::TurnBegin { routing_token, .. } = &transfer.begin else {
        return false;
    };
    let mut retained = evaluate.clone();
    if retained
        .begun_grant_id
        .as_ref()
        .is_some_and(|grant| grant != &prepared.grant_id)
    {
        return false;
    }
    retained.begun_grant_id = None;
    image.dispatch_generation.checked_add(1) == Some(successor.owner_generation)
        && (if head.promoted_message_index.is_some() {
            transfer.evaluate.operation_generation == Some(image.dispatch_generation)
        } else {
            image.dispatch_generation == history.owner_generation
        })
        && successor.prompt_id.as_deref() == Some(head.pending_prompt.id.as_str())
        && head.pending_prompt.id == transfer.prompt_id
        && containment_prompt_fingerprint(head) == transfer.intent_fingerprint
        && head.engram_bind.is_none()
        && image
            .queued_prompts
            .iter()
            .skip(1)
            .all(|head| !head.has_engram_intent())
        && serde_json::to_value(retained).ok() == serde_json::to_value(&transfer.evaluate).ok()
        && image.routing_token.as_ref() == Some(routing_token)
        && (image.active_grant_id.as_ref() == Some(&prepared.grant_id)
            || image.uncertain_grant_id.as_ref() == Some(&prepared.grant_id)
            || image.begin_requested.as_ref() == Some(&prepared.grant_id))
}

fn containment_admission_transfer_materialized(
    inner: &StateInner,
    entry: &EngramSessionObligation,
) -> bool {
    if !containment_admission_transfer_matches(entry) {
        return false;
    }
    let Ok(transfer) = serde_json::from_value::<EngramContainmentAdmissionTransfer>(
        entry.settlement_evidence.clone(),
    ) else {
        return false;
    };
    // This marker is committed with the exact successor's acknowledged Begin
    // result and ordinary grant owner. It is not reconstructed from history
    // or a missing head, and it makes no claim that provider delivery happened.
    if transfer.progress.is_some() {
        return true;
    }
    let Some(record) = inner
        .find_session_index(&entry.local_session_id)
        .map(|index| &inner.sessions[index])
    else {
        return false;
    };
    let Some(head) = record.queued_prompts.front() else {
        return false;
    };
    let Some(prepared) = transfer.evaluate.prepared_begin.as_ref() else {
        return false;
    };
    let EngramControlRequest::TurnBegin { routing_token, .. } = &transfer.begin else {
        return false;
    };
    let original_generation = transfer.evaluate.operation_generation.or_else(|| {
        let EngramControlRequest::TurnEvaluate {
            idempotency_key, ..
        } = &transfer.evaluate.request
        else {
            return None;
        };
        legacy_queued_engram_operation_generation(&entry.local_session_id, idempotency_key)
    });
    let settings_match = engram_project_for_session_locked(inner, &entry.local_session_id)
        .and_then(|project| project.engram.as_ref())
        .is_some_and(|settings| {
            entry.authority_confirmed
                && settings.is_control_enabled()
                && settings.authority_store_key.as_ref() == Some(&entry.authority)
                && settings.same_admission_settings(&transfer.evaluate.settings)
        });
    settings_match
        && !record.runtime_stop_in_progress
        && !record.engram.project_reset_in_progress
        && entry.prompt_id.as_deref() == Some(head.pending_prompt.id.as_str())
        && transfer.intent_fingerprint == containment_prompt_fingerprint(head)
        && head.engram_bind.is_none()
        && !head.engram_ordinary_handoff
        && serde_json::to_value(&head.engram_evaluate).ok()
            == serde_json::to_value(Some(&transfer.evaluate)).ok()
        && record.engram.routing_token.as_ref() == Some(routing_token)
        && record.engram.active_grant_id.is_none()
        && record
            .engram
            .uncertain_grant_id
            .as_ref()
            .is_none_or(|grant| grant == &prepared.grant_id)
        && (if head.promoted_message_index.is_some() {
            original_generation == Some(record.engram.dispatch_generation)
        } else {
            record.engram.dispatch_generation == entry.owner_generation
        })
}

fn record_contained_admission_begun_progress(inner: &mut StateInner, session_id: &str) {
    let Some(index) = inner.find_session_index(session_id) else {
        return;
    };
    let record = &inner.sessions[index];
    let Some(head) = record.queued_prompts.front() else {
        return;
    };
    let Some(evaluate) = &head.engram_evaluate else {
        return;
    };
    let Some(grant) = &record.engram.active_grant_id else {
        return;
    };
    if evaluate.begun_grant_id.as_ref() != Some(grant)
        || record.engram.active_turn_intent_fingerprint.as_deref()
            != Some(containment_prompt_fingerprint(head).as_str())
    {
        return;
    }
    for entry in &mut inner.engram_session_obligations.entries {
        if entry.local_session_id != session_id
            || !inner.engram_confirmed_containment_ids.contains(&entry.id)
            || !containment_admission_transfer_matches(entry)
        {
            continue;
        }
        let Ok(mut transfer) = serde_json::from_value::<EngramContainmentAdmissionTransfer>(
            entry.settlement_evidence.clone(),
        ) else {
            continue;
        };
        let mut successor = evaluate.clone();
        successor.begun_grant_id = None;
        if transfer.progress.is_some()
            || transfer.prompt_id != head.pending_prompt.id
            || transfer.intent_fingerprint != containment_prompt_fingerprint(head)
            || serde_json::to_value(&successor).ok()
                != serde_json::to_value(&transfer.evaluate).ok()
        {
            continue;
        }
        let EngramControlRequest::TurnBegin {
            grant_id,
            idempotency_key,
            ..
        } = &transfer.begin
        else {
            continue;
        };
        if grant_id != grant {
            continue;
        }
        transfer.progress = Some(EngramContainmentAdmissionProgress {
            operation: "begun_successor".into(),
            prompt_id: transfer.prompt_id.clone(),
            intent_fingerprint: transfer.intent_fingerprint.clone(),
            grant_id: grant_id.clone(),
            begin_key: idempotency_key.clone(),
        });
        entry.settlement_evidence = serde_json::to_value(transfer).expect("typed begun progress");
    }
}
