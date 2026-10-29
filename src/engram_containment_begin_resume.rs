// Owns explicit Resume reservation for an acknowledged containment transfer's
// exact unknown Begin. Does not reopen admission, settle grants or send RPCs.

struct EngramContainedBeginReplayPermit {
    state_address: usize,
    session_id: String,
    owner: EngramQueuedAdmissionOwner,
    runtime: RuntimeToken,
    nonce: String,
    entry: Value,
    content: Value,
}

thread_local! {
    static ENGRAM_CONTAINED_BEGIN_REPLAY: std::cell::RefCell<Option<EngramContainedBeginReplayPermit>>
        = const { std::cell::RefCell::new(None) };
}

struct EngramContainedBeginReplayScope {
    state: Arc<StateMutex<StateInner>>,
    session_id: String,
    nonce: String,
    installed: bool,
    _same_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for EngramContainedBeginReplayScope {
    fn drop(&mut self) {
        if self.installed {
            ENGRAM_CONTAINED_BEGIN_REPLAY.with(|slot| { slot.borrow_mut().take(); });
        }
        let mut inner = self.state.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(&self.session_id)
            && inner.sessions[index].engram.containment_resume_owner.as_ref() == Some(&self.nonce)
        {
            inner.sessions[index].engram.containment_resume_owner = None;
        }
    }
}

// Select only the currently retained successor, never a historical Confirmed
// entry or an arbitrary uncertain grant. The newest card is authoritative:
// looking past a later refusal would resurrect an old transport failure.
fn containment_begin_replay_entry<'a>(
    inner: &'a StateInner,
    session_id: &str,
    nonce: Option<&str>,
) -> Option<&'a EngramSessionObligation> {
    let record = &inner.sessions[inner.find_visible_session_index(session_id)?];
    let head = record.queued_prompts.front()?;
    if record.engram.containment_resume_owner.as_deref() != nonce
        || engram_session_obligation_locked(inner, session_id).is_some()
        || !head.engram_interrupted || !head.promotion_disposition_known
        || head.engram_ordinary_handoff || queued_prompt_held_for_durability(record, head)
        || record.engram.active_grant_id.is_some()
        || record.engram.abort_retry.is_some() || record.engram.admission_retry.is_some()
        || record.engram.bind_retry.is_some() || engram_retry_attempt_in_flight(record)
        || record.engram.bind_in_progress || record.engram.checkpoint_in_progress
        || record.engram.project_reset_in_progress || record.runtime_stop_in_progress
        || record.engram.source_observation_gate.is_some() || record.engram_boot_recovery_pending
        || record.engram.disabled_reason.is_some() || record.dedicated_cleanup_holds_admission()
        || !matches!(record.session.status, SessionStatus::Idle | SessionStatus::Error)
        || record.runtime.runtime_token().is_none()
    { return None; }
    let prepared = engram_begin_replay_pending(record)?;
    let evaluate = head.engram_evaluate.as_ref()?;
    if evaluate.begun_grant_id.is_some() || head.engram_bind.is_some() { return None; }
    let card = record.session.messages.iter().rev().find_map(|message| match message {
        Message::EngramControl { card, .. } => Some(card), _ => None,
    })?;
    let cause = card.causal_failure.as_ref()?;
    if !engram_cause_allows_begin_replay(cause)
        || cause.attempt_id.as_deref() != Some(prepared.idempotency_key.as_str())
    { return None; }
    let operation_generation = evaluate.operation_generation.or_else(|| match &evaluate.request {
        EngramControlRequest::TurnEvaluate { idempotency_key, .. } =>
            legacy_queued_engram_operation_generation(session_id, idempotency_key),
        _ => None,
    });
    let current = AppState::engram_binding_target_for_session_shape_locked(inner, session_id, true)
        .ok().flatten()?;
    inner.engram_session_obligations.entries.iter().find(|entry| {
        if entry.local_session_id != session_id
            || entry.phase != EngramSessionObligationPhase::Confirmed
            || !inner.engram_confirmed_containment_ids.contains(&entry.id)
            || !containment_admission_transfer_matches(entry)
            || current.connection != entry.connection
            || current.settings.authority_store_key.as_ref() != Some(&entry.authority)
            || !current.settings.same_admission_settings(&evaluate.settings)
            || current.routing_token.as_ref() != match &evaluate.request {
                EngramControlRequest::TurnEvaluate { routing_token, .. } => Some(routing_token),
                _ => None,
            }
            || operation_generation != Some(record.engram.dispatch_generation)
        { return false; }
        let Ok(transfer) = serde_json::from_value::<EngramContainmentAdmissionTransfer>(
            entry.settlement_evidence.clone()) else { return false; };
        transfer.prompt_id == head.pending_prompt.id
            && transfer.intent_fingerprint == containment_prompt_fingerprint(head)
            && serde_json::to_value(evaluate).ok() == serde_json::to_value(&transfer.evaluate).ok()
    })
}

fn containment_begin_replay_resume_allowed(inner: &StateInner, session_id: &str) -> bool {
    ENGRAM_CONTAINED_BEGIN_REPLAY.with(|slot| slot.borrow().as_ref().is_some_and(|permit| {
        permit.state_address == inner as *const StateInner as usize
            && permit.session_id == session_id
            && containment_begin_replay_entry(inner, session_id, Some(&permit.nonce))
                .is_some_and(|entry| serde_json::to_value(entry).ok().as_ref() == Some(&permit.entry))
            && inner.find_session_index(session_id).is_some_and(|index| {
                let record = &inner.sessions[index];
                permit.owner.matches(record)
                    && record.runtime.matches_runtime_token(&permit.runtime)
                    && engram_admission_live_content(record) == permit.content
            })
    }))
}

impl AppState {
    fn begin_contained_begin_replay_resume(
        &self, session_id: &str,
    ) -> Result<Option<EngramContainedBeginReplayScope>, ApiError> {
        if ENGRAM_CONTAINED_BEGIN_REPLAY.with(|slot| slot.borrow().is_some()) {
            return Err(ApiError::conflict("Engram exact Begin replay already has a Resume owner"));
        }
        let (permit, image) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_visible_session_index(session_id) else { return Ok(None); };
            if inner.sessions[index].engram.containment_resume_owner.is_some() {
                return Err(ApiError::conflict("Engram recovery already has a Resume owner"));
            }
            let Some(entry) = containment_begin_replay_entry(&inner, session_id, None) else { return Ok(None); };
            let record = &inner.sessions[index];
            let permit = EngramContainedBeginReplayPermit {
                state_address: &*inner as *const StateInner as usize,
                session_id: session_id.to_owned(),
                owner: EngramQueuedAdmissionOwner::capture(record).expect("retained head"),
                runtime: record.runtime.runtime_token().expect("runtime checked"),
                nonce: Uuid::new_v4().to_string(),
                entry: serde_json::to_value(entry).expect("validated entry"),
                content: engram_admission_live_content(record),
            };
            let image = EngramProjectContainmentFence::capture(&inner, &entry.project_id, &[session_id.to_owned()])
                .map_err(|_| ApiError::conflict("Exact Begin recovery durability image is unavailable"))?;
            inner.sessions[index].engram.containment_resume_owner = Some(permit.nonce.clone());
            (permit, image)
        };
        let mut scope = EngramContainedBeginReplayScope {
            state: self.inner.clone(), session_id: session_id.to_owned(), nonce: permit.nonce.clone(),
            installed: false, _same_thread: std::marker::PhantomData,
        };
        self.confirm_engram_containment(image)?;
        ENGRAM_CONTAINED_BEGIN_REPLAY.with(|slot| *slot.borrow_mut() = Some(permit));
        scope.installed = true;
        if !containment_begin_replay_resume_allowed(&self.inner.lock().expect("state mutex poisoned"), session_id) {
            return Err(ApiError::conflict("Exact Begin recovery owner changed before acknowledgement"));
        }
        Ok(Some(scope))
    }
}
