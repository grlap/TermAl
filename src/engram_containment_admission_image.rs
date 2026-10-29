// Owns strict original-admission and durable successor evidence validation.
// Split from the new engram_containment_admission.rs implementation.
// Does not replay operations, settle independent grants or authorize dispatch.

fn containment_prompt_fingerprint(head: &QueuedPromptRecord) -> String {
    engram_turn_intent_fingerprint(
        &head.pending_prompt.text,
        head.pending_prompt.expanded_text.as_deref(),
        &head.attachments,
        head.pending_prompt.source.as_ref(),
        head.source,
    )
}

// This is a validated durable link, not a reason-code exception. An image
// containing any independent grant, checkpoint or possibly sent Begin cannot
// be handed to admission recovery.
fn containment_original_admission(
    entry: &EngramSessionObligation,
) -> Option<EngramUnsettledControlImage> {
    if entry.kind != EngramSessionObligationKind::CheckpointContainment
        || !entry.authority_confirmed
        || entry.basis != EngramSessionObligationBasis::ControlStateUnsettled
    {
        return None;
    }
    let image: EngramUnsettledControlImage =
        serde_json::from_value(entry.frozen_request.clone()).ok()?;
    let head = image.queued_prompts.first()?;
    if image.operation != "unsettled_control_image"
        || serde_json::to_value(&image).ok()? != entry.frozen_request
        || image.active_grant_id.is_some()
        || image.uncertain_grant_id.is_some()
        || image.checkpoint_in_progress
        || image.runtime_running
        || image.bind_in_progress
        || image.begin_requested.is_some()
        || !entry.grant_id.is_empty()
        || image.dispatch_generation.checked_add(1)? != entry.owner_generation
        || image.routing_token.as_deref().unwrap_or_default() != entry.routing_token
        || head.engram_ordinary_handoff
        || entry.prompt_id.as_deref() != Some(head.pending_prompt.id.as_str())
        || image
            .queued_prompts
            .iter()
            .skip(1)
            .any(QueuedPromptRecord::has_engram_intent)
        || (head.engram_bind.is_some() == head.engram_evaluate.is_some())
    {
        return None;
    }
    let (connection, settings) = if let Some(bind) = &head.engram_bind {
        if !image.begins_recorded && image.routing_token.is_some() {
            return None;
        }
        match &bind.request {
            EngramControlRequest::SessionBind {
                external_ref,
                idempotency_key,
                ..
            } if external_ref == &entry.external_ref && !idempotency_key.trim().is_empty() => {}
            _ => return None,
        }
        (&bind.connection, &bind.settings)
    } else {
        let evaluate = head.engram_evaluate.as_ref()?;
        if !image.begins_recorded
            || evaluate.begun_grant_id.is_some()
            || evaluate.prepared_begin.is_some()
        {
            return None;
        }
        match &evaluate.request {
            EngramControlRequest::TurnEvaluate {
                routing_token,
                idempotency_key,
                intent_fingerprint,
                ..
            } if !routing_token.trim().is_empty()
                && !idempotency_key.trim().is_empty()
                && intent_fingerprint == &containment_prompt_fingerprint(head)
                && image.routing_token.as_ref() == Some(routing_token) => {}
            _ => return None,
        }
        (&evaluate.connection, &evaluate.settings)
    };
    if connection != &entry.connection
        || !settings.is_control_enabled()
        || settings.authority_store_key.as_ref() != Some(&entry.authority)
    {
        return None;
    }
    Some(image)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramContainmentAdmissionTransfer {
    operation: String,
    prompt_id: String,
    intent_fingerprint: String,
    evaluate: EngramQueuedEvaluate,
    begin: EngramControlRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress: Option<EngramContainmentAdmissionProgress>,
}

fn containment_admission_transfer_matches(entry: &EngramSessionObligation) -> bool {
    let Some(original) = containment_original_admission(entry) else {
        return false;
    };
    let Ok(transfer) = serde_json::from_value::<EngramContainmentAdmissionTransfer>(
        entry.settlement_evidence.clone(),
    ) else {
        return false;
    };
    let head = &original.queued_prompts[0];
    let evaluate = &transfer.evaluate;
    let original_settings = head
        .engram_evaluate
        .as_ref()
        .map(|e| &e.settings)
        .or_else(|| head.engram_bind.as_ref().map(|b| &b.settings));
    let Some(prepared) = &evaluate.prepared_begin else {
        return false;
    };
    let EngramControlRequest::TurnEvaluate {
        routing_token,
        intent_fingerprint,
        idempotency_key,
        ..
    } = &evaluate.request
    else {
        return false;
    };
    let EngramControlRequest::TurnBegin {
        routing_token: begin_token,
        grant_id,
        delivery_tokens,
        idempotency_key: begin_key,
    } = &transfer.begin
    else {
        return false;
    };
    transfer.operation == "retained_admission_transfer"
        && serde_json::to_value(&transfer).ok().as_ref() == Some(&entry.settlement_evidence)
        && transfer.prompt_id == head.pending_prompt.id
        && transfer.intent_fingerprint == containment_prompt_fingerprint(head)
        && transfer.intent_fingerprint == *intent_fingerprint
        && evaluate.connection == entry.connection
        && original_settings.is_some_and(|s| s.same_admission_settings(&evaluate.settings))
        && evaluate.begun_grant_id.is_none()
        && !idempotency_key.trim().is_empty()
        && !begin_key.trim().is_empty()
        && !routing_token.trim().is_empty()
        && routing_token == begin_token
        && !grant_id.trim().is_empty()
        && grant_id != ENGRAM_UNCERTAIN_GRANT_UNKNOWN
        && prepared.grant_id == *grant_id
        && prepared.idempotency_key == *begin_key
        && prepared.delivery_tokens == *delivery_tokens
        && transfer.progress.as_ref().is_none_or(|progress| {
            progress.operation == "begun_successor"
                && progress.prompt_id == transfer.prompt_id
                && progress.intent_fingerprint == transfer.intent_fingerprint
                && progress.grant_id == *grant_id
                && progress.begin_key == *begin_key
        })
        && chrono::DateTime::parse_from_rfc3339(&prepared.issued_no_later_than).is_ok()
        && head.engram_evaluate.as_ref().is_none_or(|original| {
            serde_json::to_value(&original.request).ok()
                == serde_json::to_value(&evaluate.request).ok()
        })
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramContainmentAdmissionDecision {
    operation: String,
    prompt_id: String,
    evaluate: EngramQueuedEvaluate,
    card: EngramControlCard,
}

fn containment_admission_decision_matches(entry: &EngramSessionObligation) -> bool {
    let Some(original) = containment_original_admission(entry) else {
        return false;
    };
    let Ok(proof) = serde_json::from_value::<EngramContainmentAdmissionDecision>(
        entry.settlement_evidence.clone(),
    ) else {
        return false;
    };
    let head = &original.queued_prompts[0];
    let evaluate = &proof.evaluate;
    let EngramControlRequest::TurnEvaluate {
        intent_fingerprint,
        idempotency_key,
        routing_token,
        ..
    } = &evaluate.request
    else {
        return false;
    };
    let settings = head
        .engram_evaluate
        .as_ref()
        .map(|e| &e.settings)
        .or_else(|| head.engram_bind.as_ref().map(|b| &b.settings));
    proof.operation == "retained_admission_decision"
        && serde_json::to_value(&proof).ok().as_ref() == Some(&entry.settlement_evidence)
        && proof.prompt_id == head.pending_prompt.id
        && evaluate.connection == entry.connection
        && settings.is_some_and(|s| s.same_admission_settings(&evaluate.settings))
        && evaluate.prepared_begin.is_none()
        && evaluate.begun_grant_id.is_none()
        && !routing_token.trim().is_empty()
        && !idempotency_key.trim().is_empty()
        && *intent_fingerprint == containment_prompt_fingerprint(head)
        && head.engram_evaluate.as_ref().is_none_or(|original| {
            serde_json::to_value(&original.request).ok()
                == serde_json::to_value(&evaluate.request).ok()
        })
        && matches!(
            proof.card.decision,
            EngramControlCardDecision::Refuse | EngramControlCardDecision::Defer
        )
        && proof.card.stage == EngramControlStage::Dispatch
        && proof.card.schema_version == ENGRAM_CONTROL_SCHEMA_VERSION
        && proof.card.dispatch == EngramControlCardDispatch::Withheld
        && proof.card.grant_id.is_none()
        && !proof.card.repair_armed
        && proof.card.fail_mode == EngramControlFailMode::Enforced
}

fn containment_admission_recovery_evidence_matches(entry: &EngramSessionObligation) -> bool {
    containment_admission_transfer_matches(entry) || containment_admission_decision_matches(entry)
}
