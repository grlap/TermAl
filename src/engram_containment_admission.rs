// Owns the scoped explicit Resume flight and prepared-Begin transfer.
// Identity and durable-link validation live in engram_containment_admission_image.rs.
// Does not authorize fresh Send or settle independent control duties.

// Only the synchronous explicit Resume flight can see this permit. Other
// threads, fresh Send/kill paths, boot and background retries retain exclusion.
// Every use rechecks the same state, ledger entry, prompt content and owner.
#[derive(Clone)]
struct EngramContainmentAdmissionPermit {
    state_address: usize,
    session_id: String,
    entry_id: String,
    entry: Value,
    prompt_id: String,
    fingerprint: String,
    generation: u64,
    replay_generation: u64,
    active_turn_generation: u64,
    original: EngramUnsettledControlImage,
    resume_owner: String,
}

thread_local! {
    static ENGRAM_CONTAINMENT_ADMISSION: std::cell::RefCell<Vec<EngramContainmentAdmissionPermit>>
        = const { std::cell::RefCell::new(Vec::new()) };
}

struct EngramContainmentAdmissionScope {
    state: Arc<StateMutex<StateInner>>,
    session_id: String,
    owner: String,
    installed: bool,
    // A scope must be dropped on the thread that installed it.
    _same_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for EngramContainmentAdmissionScope {
    fn drop(&mut self) {
        if self.installed {
            ENGRAM_CONTAINMENT_ADMISSION.with(|permits| {
                permits.borrow_mut().pop();
            });
        }
        let mut inner = self.state.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(&self.session_id)
            && inner.sessions[index]
                .engram
                .containment_resume_owner
                .as_deref()
                == Some(self.owner.as_str())
        {
            inner.sessions[index].engram.containment_resume_owner = None;
        }
    }
}

fn containment_admission_permits(
    inner: &StateInner,
    entry: &EngramSessionObligation,
    session_id: &str,
) -> bool {
    ENGRAM_CONTAINMENT_ADMISSION.with(|permits| {
        let permits = permits.borrow();
        permits.last().is_some_and(|permit| {
            containment_admission_permit_matches(inner, entry, session_id, permit)
        })
    })
}

fn containment_admission_permit_matches(
    inner: &StateInner,
    entry: &EngramSessionObligation,
    session_id: &str,
    permit: &EngramContainmentAdmissionPermit,
) -> bool {
    let Some(index) = inner.find_session_index(session_id) else {
        return false;
    };
    let record = &inner.sessions[index];
    let Some(head) = record.queued_prompts.front() else {
        return false;
    };
    let settings =
        engram_project_for_session_locked(inner, session_id).and_then(|p| p.engram.as_ref());
    let promoted = record.engram.pending_dispatch.as_ref().is_some_and(|p| {
        p.dispatch_generation == permit.replay_generation
            && p.intent_fingerprint == permit.fingerprint
    });
    let saved = &permit.original.queued_prompts[0];
    let saved_settings = saved
        .engram_bind
        .as_ref()
        .map(|b| &b.settings)
        .or_else(|| saved.engram_evaluate.as_ref().map(|e| &e.settings));
    let operation_matches = if let Some(original) = &saved.engram_evaluate {
        head.engram_bind.is_none()
            && head.engram_evaluate.as_ref().is_some_and(|current| {
                current.connection == original.connection
                    && current.settings.same_admission_settings(&original.settings)
                    && serde_json::to_value(&current.request).ok()
                        == serde_json::to_value(&original.request).ok()
            })
    } else if let Some(original) = &saved.engram_bind {
        match &head.engram_bind {
                Some(current) => entry.settlement_evidence.is_null()
                    && current.connection == original.connection
                    && current.settings.same_admission_settings(&original.settings)
                    && serde_json::to_value(&current.request).ok() == serde_json::to_value(&original.request).ok(),
                None => record.engram.begins_recorded && record.engram.routing_token.is_some()
                    && match &head.engram_evaluate {
                        None => entry.settlement_evidence.is_null(),
                        Some(current) => containment_bind_successor(entry).is_some_and(|expected|
                            current.connection == expected.connection
                                && current.settings.same_admission_settings(&expected.settings)
                                && current.operation_generation == expected.operation_generation
                                && serde_json::to_value(&current.request).ok() == serde_json::to_value(&expected.request).ok()
                                && matches!(&current.request, EngramControlRequest::TurnEvaluate { routing_token, .. }
                                    if record.engram.routing_token.as_ref() == Some(routing_token))),
                    },
            }
    } else {
        false
    };
    permit.state_address == inner as *const StateInner as usize
        && record.engram.containment_resume_owner.as_deref() == Some(permit.resume_owner.as_str())
        && permit.session_id == session_id
        && permit.entry_id == entry.id
        && serde_json::to_value(entry).ok().as_ref() == Some(&permit.entry)
        && permit.prompt_id == head.pending_prompt.id
        && permit.fingerprint == containment_prompt_fingerprint(head)
        && operation_matches
        && ((record.engram.dispatch_generation == permit.generation
            && record.active_turn_generation == permit.active_turn_generation)
            || (promoted
                && record.engram.dispatch_generation == permit.replay_generation
                && Some(record.active_turn_generation)
                    == permit.active_turn_generation.checked_add(1)))
        && !record.runtime_stop_in_progress
        && !record.engram.project_reset_in_progress
        && !record.engram.checkpoint_in_progress
        && record.engram.active_grant_id.is_none()
        && record.engram.uncertain_grant_id.is_none()
        && settings.is_some_and(|s| {
            s.is_control_enabled()
                && s.authority_store_key.as_ref() == Some(&entry.authority)
                && saved_settings.is_some_and(|saved| saved.same_admission_settings(s))
        })
}

fn engram_admission_obligation_locked<'a>(
    inner: &'a StateInner,
    session_id: &str,
) -> Option<&'a EngramSessionObligation> {
    inner
        .engram_session_obligations
        .all_protections()
        .find(|entry| {
            engram_obligation_requires_exclusion(inner, entry)
                && engram_obligation_matches_session(inner, entry, session_id)
                && !containment_admission_permits(inner, entry, session_id)
        })
}

fn require_no_engram_admission_obligation(
    inner: &StateInner,
    session_id: &str,
) -> Result<(), ApiError> {
    if engram_admission_obligation_locked(inner, session_id).is_some() {
        return require_no_engram_session_obligation(inner, session_id);
    }
    Ok(())
}

// A retained-transport replay crosses only these pre-promotion guards. It
// does not change the generic recovered flag or erase prior failure facts.
fn containment_retained_reconciliation_allowed(inner: &StateInner, session_id: &str) -> bool {
    let Some(entry) = engram_session_obligation_locked(inner, session_id) else {
        return false;
    };
    let Some(index) = inner.find_session_index(session_id) else {
        return false;
    };
    let record = &inner.sessions[index];
    entry.phase == EngramSessionObligationPhase::Intended
        && containment_admission_permits(inner, entry, session_id)
        && record.engram.active_grant_id.is_none()
        && record.engram.uncertain_grant_id.is_none()
        && record.engram.disabled_reason.is_none()
        && record.engram.abort_retry.is_none()
        && record.engram.admission_retry.is_none()
        && record.engram.bind_retry.is_none()
        && record.engram.source_observation_gate.is_none()
        && !record.engram.checkpoint_in_progress
        && !record.runtime_stop_in_progress
        && !record.engram_boot_recovery_pending
        && !matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
        )
        && record.queued_prompts.front().is_some_and(|head| {
            !queued_prompt_held_for_durability(record, head)
                && !head.engram_ordinary_handoff
                && head.engram_evaluate.as_ref().is_none_or(|evaluate| {
                    evaluate.begun_grant_id.is_none() && evaluate.prepared_begin.is_none()
                })
        })
}

impl AppState {
    fn contained_admission_decision(
        &self,
        inner: &StateInner,
        session_id: &str,
        card: &EngramControlCard,
    ) -> Option<(EngramSessionObligation, EngramSessionObligation)> {
        let entry = engram_session_obligation_locked(inner, session_id)?;
        if !containment_admission_permits(inner, entry, session_id) {
            return None;
        }
        let record = &inner.sessions[inner.find_session_index(session_id)?];
        if record.engram.active_grant_id.is_some() || record.engram.uncertain_grant_id.is_some() {
            return None;
        }
        let pending = record.engram.pending_dispatch.as_ref()?;
        if !matches!(
            (&pending.evaluated, card.decision),
            (
                EngramDispatchEvaluation::Refuse { .. },
                EngramControlCardDecision::Refuse
            ) | (
                EngramDispatchEvaluation::Defer { .. },
                EngramControlCardDecision::Defer
            )
        ) {
            return None;
        }
        let head = record.queued_prompts.front()?;
        let evaluate = head.engram_evaluate.clone()?;
        let mut confirmed = entry.clone();
        confirmed.phase = EngramSessionObligationPhase::Confirmed;
        confirmed.settlement_evidence = serde_json::to_value(EngramContainmentAdmissionDecision {
            operation: "retained_admission_decision".into(),
            prompt_id: head.pending_prompt.id.clone(),
            evaluate,
            card: card.clone(),
        })
        .ok()?;
        containment_admission_decision_matches(&confirmed).then(|| (entry.clone(), confirmed))
    }

    fn restore_contained_engram_gating(
        &self,
        project_snapshot: &Project,
        settings: EngramProjectSettings,
        expected_host: Option<&EngramHostSettings>,
    ) -> Result<StateResponse, ApiError> {
        let authority = validate_engram_containment_authority(project_snapshot, &settings)?;
        if !authority.confirmed || settings.authority_store_key.as_ref() != Some(&authority.route) {
            return Err(ApiError::conflict(
                "Engram recovery cannot replace its original authority",
            ));
        }
        let image = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !inner
                .find_project(&project_snapshot.id)
                .is_some_and(|p| project_matches_engram_reset_snapshot(p, project_snapshot))
                || inner.engram_project_resets.contains(&project_snapshot.id)
                || expected_host.is_some_and(|s| inner.preferences.engram != *s)
            {
                return Err(ApiError::conflict(
                    "Engram project changed before recovery settings could be saved",
                ));
            }
            let ids = inner
                .engram_session_obligations
                .entries
                .iter()
                .filter(|e| e.project_id == project_snapshot.id)
                .map(|e| e.local_session_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|id| inner.find_session_index(id).is_some())
                .collect::<Vec<_>>();
            inner
                .projects
                .iter_mut()
                .find(|p| p.id == project_snapshot.id)
                .expect("project")
                .engram = Some(settings);
            if let Err(error) = self.commit_locked(&mut inner) {
                inner
                    .projects
                    .iter_mut()
                    .find(|p| p.id == project_snapshot.id)
                    .expect("project")
                    .engram = project_snapshot.engram.clone();
                return Err(ApiError::internal(format!(
                    "failed persisting Engram recovery settings: {error:#}"
                )));
            }
            EngramProjectContainmentFence::capture(&inner, &project_snapshot.id, &ids).map_err(
                |_| ApiError::conflict("Engram recovery durability image is unavailable"),
            )?
        };
        self.confirm_engram_containment(image)?;
        Ok(self.snapshot())
    }

    fn begin_contained_admission_resume(
        &self,
        session_id: &str,
    ) -> Result<Option<EngramContainmentAdmissionScope>, ApiError> {
        if ENGRAM_CONTAINMENT_ADMISSION.with(|permits| !permits.borrow().is_empty()) {
            return Err(ApiError::conflict(
                "Engram containment admission already has an explicit Resume owner on this thread",
            ));
        }
        self.retry_containment_admission_transfer(session_id)?;
        let (permit, image) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let entries = inner
                .engram_session_obligations
                .entries
                .iter()
                .filter(|e| {
                    engram_obligation_requires_exclusion(&inner, e)
                        && engram_obligation_matches_session(&inner, e, session_id)
                })
                .collect::<Vec<_>>();
            if entries.is_empty() {
                return Ok(None);
            }
            if entries.len() != 1 {
                require_no_engram_session_obligation(&inner, session_id)?;
            }
            let entry = entries[0].clone();
            let original = containment_original_admission(&entry).ok_or_else(||
                ApiError::conflict("Engram containment retains an independent control duty; admission recovery is unavailable"))?;
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = &inner.sessions[index];
            let head = record
                .queued_prompts
                .front()
                .ok_or_else(|| ApiError::conflict("retained admission head is missing"))?;
            let saved = &original.queued_prompts[0];
            let successor = containment_bind_successor(&entry);
            let request_matches = if let Some(successor) = &successor {
                head.engram_bind.is_none()
                    && serde_json::to_value(&head.engram_evaluate).ok()
                        == serde_json::to_value(Some(successor)).ok()
                    && matches!(&successor.request, EngramControlRequest::TurnEvaluate { routing_token, .. }
                        if record.engram.routing_token.as_ref() == Some(routing_token))
            } else {
                entry.settlement_evidence.is_null()
                    && serde_json::to_value(&head.engram_bind).ok()
                        == serde_json::to_value(&saved.engram_bind).ok()
                    && serde_json::to_value(&head.engram_evaluate).ok()
                        == serde_json::to_value(&saved.engram_evaluate).ok()
            };
            let settings = engram_project_for_session_locked(&inner, session_id)
                .and_then(|p| p.engram.as_ref());
            if entry.phase != EngramSessionObligationPhase::Intended
                || record.engram.dispatch_generation != entry.owner_generation
                || record.engram.active_grant_id.is_some()
                || record.engram.uncertain_grant_id.is_some()
                || record.engram.pending_dispatch.is_some()
                || record.engram.admission_in_progress.is_some()
                || record.engram.containment_resume_owner.is_some()
                || record.engram.admission_retry.is_some()
                || record.engram.abort_retry.is_some()
                || record.engram.bind_retry.is_some()
                || record.runtime_stop_in_progress
                || record.engram.project_reset_in_progress
                || record.engram.bind_in_progress
                || record.engram.checkpoint_in_progress
                || head.pending_prompt.id != saved.pending_prompt.id
                || containment_prompt_fingerprint(head) != containment_prompt_fingerprint(saved)
                || !request_matches
                || !settings.is_some_and(|s| {
                    s.is_control_enabled()
                        && s.authority_store_key.as_ref() == Some(&entry.authority)
                        && saved
                            .engram_bind
                            .as_ref()
                            .is_none_or(|b| b.settings.same_admission_settings(s))
                        && saved
                            .engram_evaluate
                            .as_ref()
                            .is_none_or(|e| e.settings.same_admission_settings(s))
                })
            {
                return Err(ApiError::conflict(
                    "Engram retained admission no longer matches its original authority, operation and owner",
                ));
            }
            let generation = head
                .engram_evaluate
                .as_ref()
                .and_then(|e| e.operation_generation)
                .or_else(|| {
                    head.engram_bind
                        .as_ref()
                        .and_then(|b| b.operation_generation)
                })
                .unwrap_or_else(|| entry.owner_generation.saturating_add(1));
            let resume_owner = Uuid::new_v4().to_string();
            let permit = EngramContainmentAdmissionPermit {
                state_address: &*inner as *const StateInner as usize,
                session_id: session_id.to_owned(),
                entry_id: entry.id.clone(),
                entry: serde_json::to_value(&entry).expect("entry"),
                prompt_id: head.pending_prompt.id.clone(),
                fingerprint: containment_prompt_fingerprint(head),
                generation: entry.owner_generation,
                replay_generation: generation,
                active_turn_generation: record.active_turn_generation,
                original: original.clone(),
                resume_owner: resume_owner.clone(),
            };
            let image = EngramProjectContainmentFence::capture(
                &inner,
                &entry.project_id,
                &[session_id.to_owned()],
            )
            .map_err(|_| {
                ApiError::conflict("retained admission durability image is unavailable")
            })?;
            inner.sessions[index].engram.containment_resume_owner = Some(resume_owner);
            (permit, image)
        };
        let mut scope = EngramContainmentAdmissionScope {
            state: self.inner.clone(),
            session_id: session_id.to_owned(),
            owner: permit.resume_owner.clone(),
            installed: false,
            _same_thread: std::marker::PhantomData,
        };
        self.confirm_engram_containment(image)?;
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let current = engram_session_obligation_locked(&inner, session_id);
            if !current.is_some_and(|entry| {
                containment_admission_permit_matches(&inner, entry, session_id, &permit)
            }) || !inner.find_session_index(session_id).is_some_and(|index| {
                !engram_retry_attempt_in_flight(&inner.sessions[index])
                    && inner.sessions[index].engram.admission_retry.is_none()
            }) {
                return Err(ApiError::conflict(
                    "Contained Resume ownership changed before its acknowledgement",
                ));
            }
        }
        ENGRAM_CONTAINMENT_ADMISSION.with(|permits| permits.borrow_mut().push(permit));
        scope.installed = true;
        Ok(Some(scope))
    }

    // Called only after the existing Evaluate path has installed its exact
    // prepared Begin. The ledger and successor are acknowledged together;
    // only then does the runtime latch allow the ordinary Begin protocol.
    fn transfer_contained_admission(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
        begin: &EngramControlRequest,
        prepared: &EngramPreparedBegin,
        original_evaluate: &Value,
    ) -> Result<bool, EngramTransportError> {
        let local = |message: &str| EngramTransportError::local_state(message);
        let (id, image, successor, confirmed_image) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(entry) = engram_session_obligation_locked(&inner, session_id).cloned() else {
                // A previously acknowledged transfer still owns this exact
                // prepared Begin. Keep that successor on a later preparation
                // failure; a different/new admission gets no such treatment.
                return Ok(inner.find_session_index(session_id).is_some_and(|index| {
                    let record = &inner.sessions[index];
                    owner.matches(record)
                        && inner
                            .engram_session_obligations
                            .entries
                            .iter()
                            .any(|entry| {
                                entry.local_session_id == session_id
                                    && inner.engram_confirmed_containment_ids.contains(&entry.id)
                                    && entry.phase == EngramSessionObligationPhase::Confirmed
                                    && containment_admission_transfer_matches(entry)
                                    && entry.settlement_evidence.get("begin")
                                        == serde_json::to_value(begin).ok().as_ref()
                                    && entry.prompt_id.as_deref() == Some(owner.prompt_id.as_str())
                                    && record
                                        .queued_prompts
                                        .front()
                                        .and_then(|head| head.engram_evaluate.as_ref())
                                        .and_then(|evaluate| evaluate.prepared_begin.as_ref())
                                        == Some(prepared)
                            })
                }));
            };
            if !containment_admission_permits(&inner, &entry, session_id) {
                return Err(local(
                    "Containment admission transfer lost its explicit Resume owner",
                ));
            }
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| local("Admission session disappeared"))?;
            let record = &inner.sessions[index];
            if !owner.matches(record) {
                return Err(local("Admission transfer lost its queue owner"));
            }
            let mut evaluate = record
                .queued_prompts
                .front()
                .and_then(|q| q.engram_evaluate.clone())
                .ok_or_else(|| local("Admission transfer has no prepared Evaluate successor"))?;
            if serde_json::to_value(&evaluate).ok().as_ref() != Some(original_evaluate) {
                return Err(local("Admission transfer's retained Evaluate changed"));
            }
            evaluate.prepared_begin = Some(prepared.clone());
            let mut confirmed = entry.clone();
            confirmed.phase = EngramSessionObligationPhase::Confirmed;
            confirmed.settlement_evidence =
                serde_json::to_value(EngramContainmentAdmissionTransfer {
                    operation: "retained_admission_transfer".to_owned(),
                    prompt_id: owner.prompt_id.clone(),
                    intent_fingerprint: containment_prompt_fingerprint(
                        record.queued_prompts.front().expect("head"),
                    ),
                    evaluate,
                    begin: begin.clone(),
                    progress: None,
                })
                .map_err(|_| local("Admission transfer cannot be encoded"))?;
            if !containment_admission_transfer_matches(&confirmed) {
                return Err(local(
                    "Admission transfer does not match the original operation and exact prepared Begin",
                ));
            }
            let confirmed_image = serde_json::to_value(&confirmed).expect("transfer");
            let mut prospective = inner.engram_session_obligations.clone();
            let slot = prospective
                .entries
                .iter_mut()
                .find(|e| e.id == entry.id)
                .expect("entry");
            *slot = confirmed;
            prospective
                .validate()
                .map_err(|_| local("Admission transfer image is invalid"))?;
            inner
                .session_mut_by_index(index)
                .expect("matched session")
                .queued_prompts
                .front_mut()
                .expect("matched head")
                .engram_evaluate
                .as_mut()
                .expect("matched Evaluate")
                .prepared_begin = Some(prepared.clone());
            inner.engram_session_obligations = prospective;
            let successor = engram_admission_live_content(&inner.sessions[index]);
            if self.commit_locked(&mut inner).is_err() {
                // A commit error does not prove the writer saw nothing. Keep
                // the exact issued-grant successor and unlatched transfer for
                // local acknowledgement recovery; never restore Intended over P.
                return Err(local(
                    "Admission transfer persistence failed; containment remains armed",
                ));
            }
            let image = EngramProjectContainmentFence::capture(
                &inner,
                &entry.project_id,
                &[session_id.to_owned()],
            )
            .map_err(|_| local("Admission transfer durability image is unavailable"))?;
            (entry.id, image, successor, confirmed_image)
        };
        self.confirm_engram_containment(image).map_err(|_| {
            local("Admission transfer durability is unconfirmed; containment remains armed")
        })?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(entry) = inner
            .engram_session_obligations
            .entries
            .iter()
            .find(|e| e.id == id)
        else {
            return Err(local(
                "Admission transfer ownership changed before acknowledgement",
            ));
        };
        if serde_json::to_value(entry).ok().as_ref() != Some(&confirmed_image)
            || !containment_admission_transfer_materialized(&inner, entry)
            || !inner.find_session_index(session_id).is_some_and(|i| {
                owner.matches(&inner.sessions[i])
                    && engram_admission_live_content(&inner.sessions[i]) == successor
            })
        {
            return Err(local(
                "Admission transfer ownership changed before acknowledgement",
            ));
        }
        inner.engram_confirmed_containment_ids.insert(id);
        Ok(true)
    }

    // An uncertain acknowledgement is retried locally. It never sends Begin,
    // recreates an Evaluate or replaces the original retained operation.
    fn retry_containment_admission_transfer(&self, session_id: &str) -> Result<(), ApiError> {
        let retry = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let entries = inner
                .engram_session_obligations
                .entries
                .iter()
                .filter(|e| {
                    engram_obligation_requires_exclusion(&inner, e)
                        && engram_obligation_matches_session(&inner, e, session_id)
                })
                .collect::<Vec<_>>();
            if entries.len() != 1 {
                return Ok(());
            }
            let entry = entries[0];
            if entry.phase != EngramSessionObligationPhase::Confirmed
                || !containment_admission_recovery_evidence_matches(entry)
            {
                return Ok(());
            }
            let record = inner
                .find_visible_session_index(session_id)
                .map(|i| &inner.sessions[i])
                .ok_or_else(|| ApiError::conflict("Engram admission transfer owner is missing"))?;
            if containment_admission_transfer_matches(entry) {
                if !containment_admission_transfer_materialized(&inner, entry) {
                    return Err(ApiError::conflict(
                        "Engram admission transfer has no current enabled successor permission",
                    ));
                }
                let transfer: EngramContainmentAdmissionTransfer =
                    serde_json::from_value(entry.settlement_evidence.clone()).map_err(|_| {
                        ApiError::conflict("Engram admission transfer proof is invalid")
                    })?;
                let head = record.queued_prompts.front().ok_or_else(|| {
                    ApiError::conflict("Engram admission transfer head is missing")
                })?;
                if head.pending_prompt.id != transfer.prompt_id
                    || containment_prompt_fingerprint(head) != transfer.intent_fingerprint
                    || serde_json::to_value(&head.engram_evaluate).ok()
                        != serde_json::to_value(Some(&transfer.evaluate)).ok()
                    || record.runtime_stop_in_progress
                    || record.engram.project_reset_in_progress
                {
                    return Err(ApiError::conflict(
                        "Engram admission transfer successor changed before acknowledgement",
                    ));
                }
            } else {
                let proof: EngramContainmentAdmissionDecision =
                    serde_json::from_value(entry.settlement_evidence.clone()).map_err(|_| {
                        ApiError::conflict("Engram admission decision proof is invalid")
                    })?;
                if record.engram.active_grant_id.is_some()
                    || record.engram.uncertain_grant_id.is_some()
                    || !record.session.messages.iter().any(|m| {
                        matches!(m,
                        Message::EngramControl { card, .. } if card == &proof.card)
                    })
                {
                    return Err(ApiError::conflict(
                        "Engram admission decision has no matching durable local transition",
                    ));
                }
            }
            (
                entry.id.clone(),
                serde_json::to_value(entry).expect("entry"),
                engram_admission_live_content(record),
                EngramProjectContainmentFence::capture(
                    &inner,
                    &entry.project_id,
                    &[session_id.to_owned()],
                )
                .map_err(|_| {
                    ApiError::conflict("Engram admission transfer durability image is unavailable")
                })?,
            )
        };
        let (id, expected_entry, successor, image) = retry;
        self.confirm_engram_containment(image)?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !inner.engram_session_obligations.entries.iter().any(|e| {
            e.id == id
                && serde_json::to_value(e).ok().as_ref() == Some(&expected_entry)
                && (!containment_admission_transfer_matches(e)
                    || containment_admission_transfer_materialized(&inner, e))
        }) || !inner
            .find_session_index(session_id)
            .is_some_and(|i| engram_admission_live_content(&inner.sessions[i]) == successor)
        {
            return Err(ApiError::conflict(
                "Engram admission transfer ownership changed during acknowledgement",
            ));
        }
        inner.engram_confirmed_containment_ids.insert(id);
        Ok(())
    }
}
