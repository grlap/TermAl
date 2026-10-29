// Owns the acknowledged same-head transition from an exact retained Bind to
// its specific Evaluate. Original Bind identity stays in the obligation.
// Does not send RPCs, settle grants, re-arm retries or admit successor prompts.

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngramContainmentBindSuccessor {
    operation: String,
    prompt_id: String,
    evaluate: EngramQueuedEvaluate,
}

fn containment_bind_successor(entry: &EngramSessionObligation) -> Option<EngramQueuedEvaluate> {
    let original = containment_original_admission(entry)?;
    let head = &original.queued_prompts[0];
    let bind = head.engram_bind.as_ref()?;
    let proof: EngramContainmentBindSuccessor =
        serde_json::from_value(entry.settlement_evidence.clone()).ok()?;
    let evaluate = &proof.evaluate;
    let EngramControlRequest::SessionBind {
        mediated_effects, ..
    } = &bind.request
    else {
        return None;
    };
    let EngramControlRequest::TurnEvaluate {
        routing_token,
        idempotency_key,
        intent_fingerprint,
        purpose,
        requested_effects,
        resource_intents,
    } = &evaluate.request
    else {
        return None;
    };
    let generation = bind
        .operation_generation
        .unwrap_or_else(|| entry.owner_generation.saturating_add(1));
    (proof.operation == "retained_bind_evaluate"
        && serde_json::to_value(&proof).ok().as_ref() == Some(&entry.settlement_evidence)
        && proof.prompt_id == head.pending_prompt.id
        && evaluate.connection == bind.connection
        && evaluate.settings.same_admission_settings(&bind.settings)
        && evaluate.operation_generation == Some(generation)
        && evaluate.prepared_begin.is_none()
        && evaluate.begun_grant_id.is_none()
        && !routing_token.trim().is_empty()
        && *intent_fingerprint == containment_prompt_fingerprint(head)
        && *idempotency_key
            == format!(
                "termal-evaluate:{}:{generation}:{intent_fingerprint}",
                entry.local_session_id
            )
        && purpose == "ordinary"
        && requested_effects == mediated_effects
        && resource_intents.is_empty())
    .then(|| proof.evaluate)
}

impl AppState {
    fn prepare_contained_bind_evaluate(
        &self,
        target: &EngramBindingTarget,
        intent: &EngramTurnIntentSnapshot,
        owner: &EngramQueuedAdmissionOwner,
        request: EngramControlRequest,
    ) -> std::result::Result<EngramControlRequest, EngramTransportError> {
        let local = || {
            EngramTransportError::local_state(
                "Retained Bind-to-Evaluate transfer lost its exact original owner",
            )
        };
        let (permit, image) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let entry = engram_session_obligation_locked(&inner, &intent.session_id)
                .cloned()
                .ok_or_else(local)?;
            if !containment_retained_reconciliation_allowed(&inner, &intent.session_id)
                || !entry.settlement_evidence.is_null()
            {
                return Err(local());
            }
            let index = inner
                .find_session_index(&intent.session_id)
                .ok_or_else(local)?;
            let record = &inner.sessions[index];
            let head = record.queued_prompts.front().ok_or_else(local)?;
            let original = containment_original_admission(&entry).ok_or_else(local)?;
            let original_bind = original.queued_prompts[0]
                .engram_bind
                .as_ref()
                .ok_or_else(local)?;
            if !owner.matches(record)
                || serde_json::to_value(&head.engram_bind).ok()
                    != serde_json::to_value(Some(original_bind)).ok()
                || head.engram_evaluate.is_some()
                || target.routing_token != record.engram.routing_token
                || !record.engram.begins_recorded
                || target.connection != entry.connection
            {
                return Err(local());
            }
            let mut successor = entry.clone();
            successor.settlement_evidence = serde_json::to_value(EngramContainmentBindSuccessor {
                operation: "retained_bind_evaluate".into(),
                prompt_id: head.pending_prompt.id.clone(),
                evaluate: EngramQueuedEvaluate {
                    connection: target.connection.clone(),
                    settings: target.settings.clone(),
                    request: request.clone(),
                    operation_generation: Some(intent.dispatch_generation),
                    prepared_begin: None,
                    begun_grant_id: None,
                },
            })
            .map_err(|_| local())?;
            let evaluate = containment_bind_successor(&successor).ok_or_else(local)?;
            let mut permit = ENGRAM_CONTAINMENT_ADMISSION
                .with(|permits| permits.borrow().last().cloned())
                .ok_or_else(local)?;
            permit.entry = serde_json::to_value(&successor).map_err(|_| local())?;
            let head = inner
                .session_mut_by_index(index)
                .expect("matched session")
                .queued_prompts
                .front_mut()
                .expect("matched head");
            // The exact original replay and its specific successor change in
            // one image: no persisted no-Bind/no-Evaluate gap is recoverable.
            head.engram_bind = None;
            head.engram_evaluate = Some(evaluate);
            *inner
                .engram_session_obligations
                .entries
                .iter_mut()
                .find(|e| e.id == entry.id)
                .expect("matched entry") = successor;
            let image = EngramProjectContainmentFence::capture(
                &inner,
                &entry.project_id,
                &[intent.session_id.clone()],
            )
            .map_err(|_| local())?;
            // An unknown write keeps both the original obligation and its
            // specific successor. A later Resume must acknowledge them again.
            self.commit_locked(&mut inner).map_err(|_| local())?;
            (permit, image)
        };
        self.confirm_engram_containment(image)
            .map_err(|_| local())?;
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let entry =
                engram_session_obligation_locked(&inner, &intent.session_id).ok_or_else(local)?;
            if !containment_admission_permit_matches(&inner, entry, &intent.session_id, &permit)
                || !inner
                    .find_session_index(&intent.session_id)
                    .is_some_and(|i| owner.matches(&inner.sessions[i]))
            {
                return Err(local());
            }
        }
        ENGRAM_CONTAINMENT_ADMISSION.with(|permits| {
            let mut permits = permits.borrow_mut();
            let current = permits.last_mut().ok_or_else(local)?;
            if current.resume_owner != permit.resume_owner {
                return Err(local());
            }
            *current = permit;
            Ok(())
        })?;
        Ok(request)
    }
}
