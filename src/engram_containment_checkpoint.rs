// Owns the durable identity of an existing closing checkpoint and its explicit
// transfer to OFF containment. Does not schedule retries or infer quiescence.

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramExistingCheckpoint {
    id: String,
    authority: String,
    connection: EngramConnectionConfig,
    settings: EngramProjectSettings,
    work_binding: Option<EngramControlWorkBinding>,
    request: EngramControlRequest,
    dispatch_generation: u64,
    active_turn_generation: u64,
    runtime_episode: String,
    checkpoint_owner_generation: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum EngramExistingCheckpointProgress {
    Retained,
    PossiblySent,
    Accepted { response: Value },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum EngramCheckpointAttemptResult {
    CertifiedNotApplied { code: String },
    Unknown { code: Option<String> },
    Refused { response: Value },
    Accepted,
}

// A refusal records only its typed diagnostic code. Private producer fields
// and arbitrary extension payloads are not checkpoint settlement evidence.
fn checkpoint_refusal_projection(code: &str) -> Result<Value> {
    if engram_containment_error_code(Some(code), "") != code || code.is_empty() {
        bail!("unsupported checkpoint refusal code")
    }
    Ok(json!({ "decision": "refuse", "code": code }))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramCheckpointAttempt {
    id: String,
    predecessor: Option<String>,
    dispatch_generation: u64,
    active_turn_generation: u64,
    runtime_episode: String,
    queued_prompt_id: Option<String>,
    queued_fingerprint: Option<String>,
    result: Option<EngramCheckpointAttemptResult>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramCheckpointOperation {
    original: EngramExistingCheckpoint,
    progress: EngramExistingCheckpointProgress,
    protection: EngramSessionObligation,
    attempts: Vec<EngramCheckpointAttempt>,
}

impl<'de> Deserialize<'de> for EngramCheckpointOperation {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize, Serialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Image {
            original: EngramExistingCheckpoint,
            progress: EngramExistingCheckpointProgress,
            protection: EngramSessionObligation,
            attempts: Vec<EngramCheckpointAttempt>,
        }
        let raw = Value::deserialize(decoder)?;
        let image: Image = serde_json::from_value(raw.clone()).map_err(serde::de::Error::custom)?;
        if serde_json::to_value(&image).map_err(serde::de::Error::custom)? != raw {
            return Err(serde::de::Error::custom(
                "unsupported existing checkpoint shape",
            ));
        }
        let operation = Self {
            original: image.original,
            progress: image.progress,
            protection: image.protection,
            attempts: image.attempts,
        };
        operation.validate().map_err(serde::de::Error::custom)?;
        Ok(operation)
    }
}

impl EngramCheckpointOperation {
    fn validate(&self) -> Result<()> {
        let original = &self.original;
        if self.attempts.is_empty() || self.attempts.len() > 65_536 {
            bail!("existing checkpoint has no supported attempt owner");
        }
        let mut ids = BTreeSet::new();
        for (index, attempt) in self.attempts.iter().enumerate() {
            if attempt.id.trim().is_empty()
                || !ids.insert(&attempt.id)
                || attempt.runtime_episode.is_empty()
                || attempt.queued_prompt_id.is_some() != attempt.queued_fingerprint.is_some()
                || attempt.predecessor.as_ref()
                    != index
                        .checked_sub(1)
                        .map(|previous| &self.attempts[previous].id)
                || matches!(&attempt.result, Some(EngramCheckpointAttemptResult::CertifiedNotApplied { code })
                    if EngramInvocationApplication::from_remote_code(Some(code)) != EngramInvocationApplication::CertifiedNotApplied)
                || matches!(&attempt.result, Some(EngramCheckpointAttemptResult::Refused { response })
                    if !matches!(parse_engram_result::<EngramTurnCheckpointResponse>(response.clone()),
                        Ok(EngramTurnCheckpointResponse::Refuse { code })
                            if checkpoint_refusal_projection(&code).ok().as_ref() == Some(response)))
            {
                bail!("invalid checkpoint attempt lineage/result");
            }
        }
        let first = &self.attempts[0];
        if first.id != original.id
            || first.dispatch_generation != original.dispatch_generation
            || first.active_turn_generation != original.active_turn_generation
            || first.runtime_episode != original.runtime_episode
        {
            bail!("checkpoint original attempt identity differs");
        }
        validate_existing_checkpoint_protection(&self.protection)?;
        if self.protection.id != original.id
            || self.protection.frozen_request != serde_json::to_value(original)?
            || self.protection.settlement_evidence
                != match &self.progress {
                    EngramExistingCheckpointProgress::Accepted { response } => response.clone(),
                    _ => Value::Null,
                }
        {
            bail!("existing checkpoint mirror/protection identity or progress differs");
        }
        if original.id.trim().is_empty()
            || original.authority.trim().is_empty()
            || original.connection.session_id.trim().is_empty()
            || !matches!(&original.request, EngramControlRequest::TurnCheckpoint {
                routing_token, grant_id, idempotency_key, ..
            } if !routing_token.trim().is_empty() && !grant_id.trim().is_empty()
                && !idempotency_key.trim().is_empty())
        {
            bail!("invalid existing checkpoint identity");
        }
        if let EngramExistingCheckpointProgress::Accepted { response } = &self.progress {
            let EngramControlRequest::TurnCheckpoint { grant_id, .. } = &original.request else {
                unreachable!()
            };
            if !matches!(parse_engram_result::<EngramTurnCheckpointResponse>(response.clone()),
                Ok(EngramTurnCheckpointResponse::Checkpointed { receipt }) if &receipt.grant_id == grant_id)
            {
                bail!("existing checkpoint progress has no matching receipt");
            }
        }
        Ok(())
    }

    // A certificate describes only its own invocation. It cannot resolve an
    // older call whose reply was lost, including an abandoned send after boot.
    fn has_uncertain_checkpoint_history(&self) -> bool {
        !self.attempts.iter().any(|attempt| matches!(attempt.result,
            Some(EngramCheckpointAttemptResult::Accepted)))
            && (self.attempts.iter().any(|attempt| matches!(attempt.result,
                Some(EngramCheckpointAttemptResult::Unknown { .. })))
                || self.attempts.iter().enumerate().any(|(index, attempt)| {
                    attempt.result.is_none() && (index + 1 < self.attempts.len()
                        || matches!(self.progress, EngramExistingCheckpointProgress::PossiblySent))
                }))
    }
}

fn validate_existing_checkpoint_protection(entry: &EngramSessionObligation) -> Result<()> {
    if entry.kind != EngramSessionObligationKind::ExistingCheckpoint
        || entry.basis != EngramSessionObligationBasis::ExistingCheckpoint
    {
        bail!("invalid existing checkpoint protection kind");
    }
    let original: EngramExistingCheckpoint = serde_json::from_value(entry.frozen_request.clone())?;
    if serde_json::to_value(&original)? != entry.frozen_request
        || original.id != entry.id
        || original.connection != entry.connection
        || original.dispatch_generation != entry.owner_generation
        || original.connection.session_id != entry.local_session_id
        || entry.project_id.trim().is_empty()
        || entry.external_ref.trim().is_empty()
        || original
            .settings
            .authority_store_key
            .as_ref()
            .is_some_and(|authority| authority != &entry.authority)
        || !matches!(&original.request, EngramControlRequest::TurnCheckpoint { routing_token, grant_id, idempotency_key, .. }
            if routing_token == &entry.routing_token && grant_id == &entry.grant_id
                && !routing_token.is_empty() && !grant_id.is_empty() && !idempotency_key.is_empty())
    {
        bail!("existing checkpoint protection does not name its exact original operation");
    }
    if !entry.settlement_evidence.is_null()
        && !matches!(parse_engram_result::<EngramTurnCheckpointResponse>(entry.settlement_evidence.clone()),
            Ok(EngramTurnCheckpointResponse::Checkpointed { receipt }) if receipt.grant_id == entry.grant_id)
    {
        bail!("existing checkpoint protection has invalid receipt");
    }
    if entry.phase == EngramSessionObligationPhase::Confirmed && entry.settlement_evidence.is_null()
    {
        bail!("existing checkpoint protection confirmation lacks receipt");
    }
    Ok(())
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramContainedCheckpointProgress {
    operation: String,
    checkpoint_id: String,
    owner_generation: u64,
    response: Value,
}

fn contained_checkpoint_progress_matches(entry: &EngramSessionObligation) -> bool {
    let Ok(image) =
        serde_json::from_value::<EngramUnsettledControlImage>(entry.frozen_request.clone())
    else {
        return false;
    };
    let Some(checkpoint) = &image.existing_checkpoint else {
        return false;
    };
    let Ok(progress) = serde_json::from_value::<EngramContainedCheckpointProgress>(
        entry.settlement_evidence.clone(),
    ) else {
        return false;
    };
    progress.operation == "existing_checkpoint_progress"
        && progress.checkpoint_id == checkpoint.original.id
        && progress.owner_generation == entry.owner_generation
        && serde_json::to_value(&progress).ok().as_ref() == Some(&entry.settlement_evidence)
        && matches!(parse_engram_result::<EngramTurnCheckpointResponse>(progress.response),
            Ok(EngramTurnCheckpointResponse::Checkpointed { receipt }) if receipt.grant_id == entry.grant_id)
}

fn contained_checkpoint_is_only_duty(image: &EngramUnsettledControlImage) -> bool {
    image.checkpoint_in_progress
        && image.existing_checkpoint.is_some()
        && image.begins_recorded
        && !image.bind_in_progress
        && !image.runtime_running
        && image.uncertain_grant_id.is_none()
        && image.begin_requested.is_none()
        && image.parked_admission_retry.is_none()
        && image.parked_bind_retry.is_none()
        && !image
            .queued_prompts
            .iter()
            .any(QueuedPromptRecord::has_engram_intent)
}

// Exact original owner or the one generation transfer frozen by OFF. Runtime
// provenance is compared while live, never reconstructed as a runtime token.
fn existing_checkpoint_owner_matches(
    inner: &StateInner,
    session_id: &str,
    original: &EngramExistingCheckpoint,
    attempt_id: &str,
) -> bool {
    existing_checkpoint_accounting_owner_matches(inner, session_id, original, attempt_id, true)
}

fn existing_checkpoint_accounting_owner_matches(
    inner: &StateInner,
    session_id: &str,
    original: &EngramExistingCheckpoint,
    attempt_id: &str,
    require_flight: bool,
) -> bool {
    let Some(index) = inner.find_session_index(session_id) else {
        return false;
    };
    let record = &inner.sessions[index];
    let Some(attempt) = record
        .engram
        .checkpoint_operation
        .as_ref()
        .and_then(|operation| operation.attempts.last())
        .filter(|attempt| attempt.id == attempt_id)
    else {
        return false;
    };
    let EngramControlRequest::TurnCheckpoint {
        grant_id,
        routing_token,
        ..
    } = &original.request
    else {
        return false;
    };
    if record
        .engram
        .checkpoint_operation
        .as_ref()
        .is_none_or(|current| {
            serde_json::to_value(&current.original).ok() != serde_json::to_value(original).ok()
        })
        || record.engram.active_grant_id.as_ref() != Some(grant_id)
        || record.engram.routing_token.as_ref() != Some(routing_token)
        || record.engram.work_binding != original.work_binding
        || record.active_turn_generation != attempt.active_turn_generation
        || format!("{:?}", record.runtime.runtime_token()) != attempt.runtime_episode
        || attempt.queued_prompt_id.as_ref().is_some_and(|id| {
            record.queued_prompts.front().is_none_or(|head| {
                &head.pending_prompt.id != id
                    || Some(containment_prompt_fingerprint(head)) != attempt.queued_fingerprint
            })
        })
        || require_flight
            && (!record.engram.checkpoint_in_progress
                || record.engram.checkpoint_owner_generation
                    != original.checkpoint_owner_generation)
    {
        return false;
    }
    if !inner
        .engram_session_obligations
        .checkpoint_operations
        .iter()
        .any(|operation| {
            serde_json::to_value(&operation.original).ok() == serde_json::to_value(original).ok()
        })
    {
        return false;
    }
    if record.engram.dispatch_generation == attempt.dispatch_generation {
        return true;
    }
    inner
        .engram_session_obligations
        .entries
        .iter()
        .any(|entry| {
            entry.local_session_id == session_id
                && entry.authority_confirmed
                && entry.owner_generation == record.engram.dispatch_generation
                && attempt.dispatch_generation.checked_add(1) == Some(entry.owner_generation)
                && serde_json::from_value::<EngramUnsettledControlImage>(
                    entry.frozen_request.clone(),
                )
                .ok()
                .is_some_and(|image| {
                    image.dispatch_generation == attempt.dispatch_generation
                        && image.existing_checkpoint.as_ref().is_some_and(|operation| {
                            serde_json::to_value(&operation.original).ok()
                                == serde_json::to_value(original).ok()
                                && operation
                                    .attempts
                                    .last()
                                    .is_some_and(|attempt| attempt.id == attempt_id)
                        })
                })
        })
}

// An exiting local caller releases only its own invocation reservation. The
// independently retained request/result remains a duty until its normal ACK.
struct EngramCheckpointFlightGuard {
    state: AppState,
    session_id: String,
    operation_id: String,
    attempt_id: String,
    owner_generation: Option<u64>,
}

impl Drop for EngramCheckpointFlightGuard {
    fn drop(&mut self) {
        let Ok(mut inner) = self.state.inner.lock() else {
            return;
        };
        let Some(index) = inner.find_session_index(&self.session_id) else {
            return;
        };
        let record = &mut inner.sessions[index];
        if record
            .engram
            .checkpoint_operation
            .as_ref()
            .is_some_and(|operation| {
                operation.original.id == self.operation_id
                    && operation
                        .attempts
                        .last()
                        .is_some_and(|attempt| attempt.id == self.attempt_id)
            })
        {
            record
                .engram
                .clear_checkpoint_if_owned_by(self.owner_generation);
        }
    }
}

// A live rejected-delivery terminalizer may retire only the head frozen by
// this checkpoint attempt. The post-retirement form is used only after the
// receipt and queue change have crossed their shared durability fence.
fn rejected_checkpoint_head_matches(
    record: &SessionRecord,
    operation: &EngramCheckpointOperation,
    owner: &RejectedEngramCheckpointOwner,
    retired: bool,
) -> bool {
    let Some(attempt) = operation.attempts.last() else { return false };
    let Some(prompt_id) = attempt.queued_prompt_id.as_ref() else { return false };
    let EngramControlRequest::TurnCheckpoint { grant_id, routing_token, next_intent, .. } =
        &operation.original.request else { return false };
    if operation.validate().is_err()
        || owner.stop.kind != RuntimeStopOwnerKind::LostRuntimeTerminalization
        || !record.runtime_stop_in_progress
        || record.runtime_stop_owner.as_ref() != Some(&owner.stop)
        || owner.stop.token.as_ref().is_none_or(|token| !record.runtime.matches_runtime_token(token))
        || owner.active_turn_generation != attempt.active_turn_generation
        || owner.delivery.dispatch_generation != attempt.dispatch_generation
        || owner.delivery.grant_id != *grant_id
        || owner.delivery.prompt_id != *prompt_id
        || record.active_turn_generation != attempt.active_turn_generation
        || format!("{:?}", record.runtime.runtime_token()) != attempt.runtime_episode
        || record.engram.routing_token.as_ref() != Some(routing_token)
        || record.engram.work_binding != operation.original.work_binding
        || *next_intent != EngramNextIntent::Wait
        || attempt.dispatch_generation.checked_add(1).is_none()
    {
        return false;
    }
    if retired {
        record.engram.dispatch_generation == attempt.dispatch_generation + 1
            && record.engram.active_grant_id.is_none()
            && !record.queued_prompts.iter().any(|queued| &queued.pending_prompt.id == prompt_id)
    } else {
        record.engram.dispatch_generation == attempt.dispatch_generation
            && record.engram.active_grant_id.as_ref() == Some(grant_id)
            && record.queued_prompts.front().is_some_and(|queued| {
                &queued.pending_prompt.id == prompt_id
                    && Some(containment_prompt_fingerprint(queued)) == attempt.queued_fingerprint
                    && queued.promoted_message_index.is_some()
                    && queued.engram_interrupted
                    && engram_turn_intent_fingerprint(
                        &queued.pending_prompt.text,
                        queued.pending_prompt.expanded_text.as_deref(),
                        &queued.attachments,
                        queued.pending_prompt.source.as_ref(),
                        queued.source,
                    ) == owner.delivery.intent_fingerprint
                    && queued.engram_evaluate.as_ref()
                        .and_then(|prepared| prepared.begun_grant_id.as_ref()) == Some(grant_id)
            })
    }
}

impl AppState {
    fn prepare_existing_checkpoint_protection(
        &self,
        inner: &StateInner,
        session_id: &str,
        target: &EngramBindingTarget,
        original: EngramExistingCheckpoint,
    ) -> std::result::Result<EngramCheckpointOperation, String> {
        let project = engram_project_for_session_locked(inner, session_id)
            .ok_or("Checkpoint project is missing")?;
        let authority = match &target.settings.authority_store_key {
            Some(route) => EngramContainmentAuthority {
                route: route.clone(),
                confirmed: true,
            },
            None => resolve_engram_containment_authority(
                &target.settings,
                &target.connection.project_file,
                &target.connection.home,
            )
            .map_err(|error| error.message)?,
        };
        let protection = EngramSessionObligation {
            id: original.id.clone(),
            project_id: project.id.clone(),
            authority: authority.route,
            authority_confirmed: authority.confirmed,
            connection: original.connection.clone(),
            external_ref: target.external_ref.clone(),
            local_session_id: session_id.to_owned(),
            owner_generation: original.dispatch_generation,
            prompt_id: None,
            routing_token: target.routing_token.clone().unwrap_or_default(),
            grant_id: target.active_grant_id.clone().unwrap_or_default(),
            kind: EngramSessionObligationKind::ExistingCheckpoint,
            basis: EngramSessionObligationBasis::ExistingCheckpoint,
            phase: EngramSessionObligationPhase::Intended,
            reason_code: "existing_checkpoint_unconfirmed".into(),
            frozen_request: serde_json::to_value(&original).map_err(|error| error.to_string())?,
            settlement_evidence: Value::Null,
        };
        let head = inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions[index].queued_prompts.front());
        let first = EngramCheckpointAttempt {
            id: original.id.clone(),
            predecessor: None,
            dispatch_generation: original.dispatch_generation,
            active_turn_generation: original.active_turn_generation,
            runtime_episode: original.runtime_episode.clone(),
            queued_prompt_id: head.map(|head| head.pending_prompt.id.clone()),
            queued_fingerprint: head.map(containment_prompt_fingerprint),
            result: None,
        };
        Ok(EngramCheckpointOperation {
            original,
            protection,
            attempts: vec![first],
            progress: EngramExistingCheckpointProgress::Retained,
        })
    }

    fn acknowledge_existing_checkpoint_settlement_with_rejected_owner(
        &self,
        session_id: &str,
        operation: &EngramCheckpointOperation,
        deadline: std::time::Instant,
        rejected_delivery_owner: Option<&RejectedEngramCheckpointOwner>,
    ) -> bool {
        self.acknowledge_existing_checkpoint_settlement_with_owners(
            session_id, operation, deadline, rejected_delivery_owner, None,
        )
    }

    fn acknowledge_existing_checkpoint_settlement_with_owners(
        &self,
        session_id: &str,
        operation: &EngramCheckpointOperation,
        deadline: std::time::Instant,
        rejected_delivery_owner: Option<&RejectedEngramCheckpointOwner>,
        pending_receipt: Option<&EngramCheckpointReceiptReservation>,
    ) -> bool {
        let original = &operation.original;
        let Some(attempt) = operation.attempts.last() else {
            return false;
        };
        let captured = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let ids = inner
                .engram_session_obligations
                .all_protections()
                .filter(|entry| {
                    entry.local_session_id == session_id
                        && entry.phase == EngramSessionObligationPhase::Confirmed
                        && (entry.kind == EngramSessionObligationKind::ExistingCheckpoint
                            && entry.id == original.id
                            || contained_checkpoint_progress_matches(entry)
                                && entry
                                    .settlement_evidence
                                    .get("checkpointId")
                                    .and_then(Value::as_str)
                                    == Some(original.id.as_str()))
                })
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>();
            let Some(project) = engram_project_for_session_locked(&inner, session_id) else {
                return false;
            };
            let Ok(mut image) = EngramProjectContainmentFence::capture(
                &inner,
                &project.id,
                &[session_id.to_owned()],
            ) else {
                return false;
            };
            if image.capture_checkpoint_effects(&inner, operation, true).is_err() {
                return false;
            }
            (ids, image)
        };
        let (ids, image) = captured;
        if self
            .confirm_engram_containment_until(image.clone(), deadline)
            .is_err()
        {
            return false;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Ok(mut current) = EngramProjectContainmentFence::capture(
            &inner,
            &image.project_id,
            &[session_id.to_owned()],
        ) else {
            return false;
        };
        if current.capture_checkpoint_effects(&inner, operation, true).is_err() {
            return false;
        }
        let adopted = inner
            .engram_session_obligations
            .entries
            .iter()
            .any(|entry| entry.id == original.id);
        let sessions_match = if adopted {
            current.sessions == image.sessions
        } else {
            // Plain envelopes appended while this ACK was pending are not
            // effects or owners of the old checkpoint. Their own normal
            // queue persistence contract remains independent of this receipt.
            let without_queue = |sessions: &[(String, Value)]| {
                sessions
                    .iter()
                    .map(|(id, row)| {
                        let mut row = row.clone();
                        if let Some(fields) = row.as_object_mut() {
                            fields.remove("queue");
                        }
                        (id.clone(), row)
                    })
                    .collect::<Vec<_>>()
            };
            without_queue(&current.sessions) == without_queue(&image.sessions)
        };
        if !sessions_match
            || current.obligations != image.obligations
            || current.checkpoint_effects != image.checkpoint_effects
            || serde_json::to_value(current.settings).ok()
                != serde_json::to_value(image.settings).ok()
        {
            return false;
        }
        // A validated partial receipt is accounted, but an enclosing duty can
        // still prohibit release. Only confirmed components acquire a latch.
        if ids.is_empty() {
            return true;
        }
        let Some(index) = inner.find_session_index(session_id) else {
            return false;
        };
        let record = &inner.sessions[index];
        let still_same_attempt =
            record
                .engram
                .checkpoint_operation
                .as_ref()
                .is_some_and(|current| {
                    current.original.id == original.id
                        && matches!(
                            (checkpoint_receipt_identity(current), checkpoint_receipt_identity(operation)),
                            (Ok(current), Ok(expected)) if current == expected
                        )
                        && current
                            .attempts
                            .last()
                            .is_some_and(|current| current.id == attempt.id)
                });
        let retained_retirement = pending_receipt.is_some_and(|reservation| {
            retired_checkpoint_receipt_matches(self, &inner, session_id, operation, reservation)
        });
        let protected_head_matches = retained_retirement || match rejected_delivery_owner {
            Some(owner) => rejected_checkpoint_head_matches(record, operation, owner, true),
            None => attempt.queued_prompt_id.as_ref().is_none_or(|id| {
                record.queued_prompts.front().is_some_and(|head| {
                    &head.pending_prompt.id == id
                        && Some(containment_prompt_fingerprint(head)) == attempt.queued_fingerprint
                })
            }),
        };
        if !still_same_attempt
            || !protected_head_matches
            || !adopted
                && record
                    .queued_prompts
                    .iter()
                    .any(QueuedPromptRecord::has_engram_intent)
            || record.engram.active_grant_id.as_deref()
                == Some(match &original.request {
                    EngramControlRequest::TurnCheckpoint { grant_id, .. } => grant_id.as_str(),
                    _ => return false,
                })
            || adopted
                && (record.engram.active_grant_id.is_some()
                    || record.engram.uncertain_grant_id.is_some()
                    || record.engram.checkpoint_in_progress
                    || record.engram.bind_in_progress
                    || record.engram.pending_dispatch.is_some()
                    || record.runtime_stop_in_progress
                    || record.engram.project_reset_in_progress)
            || record.active_turn_generation != attempt.active_turn_generation
            || !retained_retirement
                && format!("{:?}", record.runtime.runtime_token()) != attempt.runtime_episode
            || adopted
                && record
                    .queued_prompts
                    .iter()
                    .any(QueuedPromptRecord::has_engram_intent)
        {
            return false;
        }
        for id in ids {
            inner.engram_confirmed_containment_ids.insert(id);
        }
        true
    }

    fn confirm_existing_checkpoint_send(
        &self,
        session_id: &str,
        operation: &EngramCheckpointOperation,
        deadline: std::time::Instant,
    ) -> std::result::Result<(), String> {
        let original = &operation.original;
        let attempt_id = &operation
            .attempts
            .last()
            .ok_or("Checkpoint attempt is missing")?
            .id;
        for possibly_sent in [false, true] {
            let image = {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                if !existing_checkpoint_owner_matches(&inner, session_id, original, attempt_id) {
                    return Err("Existing checkpoint owner changed before transport".into());
                }
                if operation.attempts.len() > 1
                    && !checkpoint_retry_configuration_matches(&inner, session_id, original)
                {
                    return Err(
                        "Checkpoint retry settings/stop owner changed before transport".into(),
                    );
                }
                let index = inner.find_session_index(session_id).expect("matched owner");
                if possibly_sent {
                    let mut operation = inner.sessions[index]
                        .engram
                        .checkpoint_operation
                        .clone()
                        .expect("owner");
                    if !matches!(
                        operation.progress,
                        EngramExistingCheckpointProgress::Retained
                    ) {
                        return Err("Existing checkpoint send boundary is already uncertain".into());
                    }
                    operation.progress = EngramExistingCheckpointProgress::PossiblySent;
                    operation.protection.phase = EngramSessionObligationPhase::Sent;
                    let mut prospective = inner.engram_session_obligations.clone();
                    let slot = prospective
                        .checkpoint_operations
                        .iter_mut()
                        .find(|current| current.original.id == original.id)
                        .ok_or("Checkpoint metadata protection disappeared")?;
                    *slot = operation.clone();
                    prospective.validate().map_err(|error| {
                        format!("Checkpoint send intent cannot be published: {error:#}")
                    })?;
                    inner.engram_session_obligations = prospective;
                    inner.session_mut_by_index(index).expect("checkpoint send owner")
                        .engram.checkpoint_operation = Some(operation);
                    self.commit_locked(&mut inner).map_err(|error| {
                        format!("Checkpoint send intent persistence is unknown: {error:#}")
                    })?;
                }
                let project_id = engram_project_for_session_locked(&inner, session_id)
                    .ok_or("Existing checkpoint project is missing")?
                    .id
                    .clone();
                EngramProjectContainmentFence::capture(
                    &inner,
                    &project_id,
                    &[session_id.to_owned()],
                )
                .map_err(|error| format!("Checkpoint durability image unavailable: {error:#}"))?
            };
            self.confirm_engram_containment_until(image, deadline)
                .map_err(|error| error.message)?;
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !existing_checkpoint_owner_matches(&inner, session_id, original, attempt_id) {
                return Err("Existing checkpoint owner changed during acknowledgement".into());
            }
            if operation.attempts.len() > 1
                && !checkpoint_retry_configuration_matches(&inner, session_id, original)
            {
                return Err(
                    "Checkpoint retry settings/stop owner changed during acknowledgement".into(),
                );
            }
        }
        Ok(())
    }
}
