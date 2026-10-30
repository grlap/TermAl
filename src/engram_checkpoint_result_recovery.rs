// Recovers the result of one immutable checkpoint, including after host restart.
// This action never binds, evaluates, begins, delivers a prompt, or certifies an
// earlier Unknown from a later invocation's busy/expired answer.

const ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT: usize = 8;
const ENGRAM_CHECKPOINT_SLOW_RETRY_SECONDS: i64 = 600;
const ENGRAM_CHECKPOINT_ATTEMPT_CAPACITY: usize = 65_536;

// Local scheduling history belongs to the existing per-original due map.
// It never records a send or grants authority to replay one.
struct EngramCheckpointRetryState {
    attempt: String,
    allocated_attempts: usize,
    accepted: bool,
    preallocation_failures: usize,
    next_at: chrono::DateTime<chrono::Utc>,
    pass: Option<Uuid>,
}

impl EngramCheckpointRetryState {
    fn new(operation: &EngramCheckpointOperation, now: chrono::DateTime<chrono::Utc>) -> Self {
        let accepted = matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. });
        Self::from_parts(&operation.original.id, operation.attempts.last().expect("validated operation").id.clone(),
            operation.attempts.len(), accepted, now)
    }

    fn from_parts(id: &str, attempt: String, allocated_attempts: usize, accepted: bool,
        now: chrono::DateTime<chrono::Utc>) -> Self {
        Self {
            attempt,
            allocated_attempts,
            accepted,
            preallocation_failures: 0,
            next_at: now + if accepted { engram_abort_retry_delay(id, 1) }
                else { checkpoint_result_retry_delay(id, allocated_attempts) },
            pass: None,
        }
    }

    fn observe(&mut self, id: &str, attempt: &str, count: usize, accepted: bool,
        now: chrono::DateTime<chrono::Utc>) {
        if count > self.allocated_attempts || accepted && !self.accepted {
            self.preallocation_failures = 0;
            self.next_at = now + if accepted { engram_abort_retry_delay(id, 1) }
                else { checkpoint_result_retry_delay(id, count) };
        }
        self.attempt = attempt.to_owned();
        self.allocated_attempts = count;
        self.accepted = accepted;
    }
}

fn checkpoint_result_retry_delay(id: &str, allocated_attempts: usize) -> chrono::Duration {
    if allocated_attempts < ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT {
        engram_abort_retry_delay(id, u32::try_from(allocated_attempts.max(1)).unwrap_or(u32::MAX))
    } else {
        // Reuse the scheduler's deterministic positive jitter (0..20%).
        let jittered = engram_abort_retry_delay(id, 6).num_milliseconds();
        chrono::Duration::milliseconds(jittered * ENGRAM_CHECKPOINT_SLOW_RETRY_SECONDS / 60)
    }
}

#[derive(Clone)]
struct EngramCheckpointResultOwner {
    operation: EngramCheckpointOperation,
    session: Option<Value>,
}

fn checkpoint_recovery_session_image(record: &SessionRecord) -> Value {
    json!({
        "runtime": format!("{:?}", record.runtime.runtime_token()),
        "turn": record.active_turn_generation,
        "dispatch": record.engram.dispatch_generation,
        "status": record.session.status,
        "routing": record.engram.routing_token,
        "grant": record.engram.active_grant_id,
        "uncertain": record.engram.uncertain_grant_id,
        "binding": record.engram.work_binding,
        "rebind": record.engram.rebind_required,
        "paused": record.orchestrator_auto_dispatch_blocked,
        "stop": record.runtime_stop_in_progress,
        "stopOwner": format!("{:?}", record.runtime_stop_owner),
        "reset": record.engram.project_reset_in_progress,
        "bind": record.engram.bind_in_progress,
        "admission": record.engram.admission_in_progress.is_some(),
        "pending": record.engram.pending_dispatch.is_some(),
        "statusOwner": record.engram.checkpoint_status_owner,
        "queue": record.queued_prompts.iter().map(|head| (
            &head.pending_prompt.id, containment_prompt_fingerprint(head)
        )).collect::<Vec<_>>(),
    })
}

fn checkpoint_recovery_project_matches(inner: &StateInner, operation: &EngramCheckpointOperation) -> bool {
    let original = &operation.original;
    inner.find_project(&operation.protection.project_id).is_some_and(|project| {
        project.remote_id == LOCAL_REMOTE_ID
            && std::path::Path::new(&project.root_path) == original.connection.project_root
            && original.connection.project_file == original.connection.project_root.join(".engram-project")
            && !inner.engram_project_resets.contains(&project.id)
            && project.engram.as_ref().is_some_and(|settings| {
                settings.is_control_enabled() && settings.same_admission_settings(&original.settings)
                    && operation.protection.authority_confirmed
                    && settings.authority_store_key.as_ref() == Some(&operation.protection.authority)
            })
    })
}

// A delivered original head may be retired only with its own receipt. A newer
// head is never consumed, even when the old checkpoint's result is recovered.
fn checkpoint_recovery_original_head(record: &SessionRecord, operation: &EngramCheckpointOperation) -> bool {
    let Some(first) = operation.attempts.first() else { return false };
    record.queued_prompts.front().is_some_and(|head| {
        first.queued_prompt_id.as_ref() == Some(&head.pending_prompt.id)
            && first.queued_fingerprint.as_ref() == Some(&containment_prompt_fingerprint(head))
            && head.engram_evaluate.as_ref().is_some_and(|evaluate| {
                evaluate.begun_grant_id.as_ref() == Some(&operation.protection.grant_id)
            })
    })
}

impl EngramCheckpointResultOwner {
    fn capture(inner: &StateInner, operation_id: &str) -> Option<Self> {
        let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
            .find(|operation| operation.original.id == operation_id)?;
        // Held and already acknowledged operations need no full report
        // validation or serialization on each idle retry tick.
        if !checkpoint_recovery_project_matches(inner, canonical)
            || canonical.protection.phase == EngramSessionObligationPhase::Confirmed
                && inner.engram_confirmed_containment_ids.contains(&canonical.original.id)
            || canonical.attempts.iter().any(|attempt| matches!(attempt.result,
                Some(EngramCheckpointAttemptResult::Refused { .. })))
            || !canonical.has_uncertain_checkpoint_history()
                && !matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. })
                && !matches!(canonical.progress, EngramExistingCheckpointProgress::Retained)
        {
            return None;
        }
        let operation = selected_checkpoint_operation(&inner.engram_session_obligations, canonical)?;
        let session = match inner.find_session_index(&operation.protection.local_session_id) {
            None => None,
            Some(index) => {
                let record = &inner.sessions[index];
                let accepted = matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. });
                let loaded_binding = matches!(record.runtime, SessionRuntime::None)
                    && record.active_turn_generation == 0 && record.engram.rebind_required
                    && record.engram.work_binding.is_none();
                if !record.is_local_session() || record.hidden
                    || matches!(record.session.status, SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping)
                    || record.runtime_stop_in_progress || record.runtime_stop_owner.is_some()
                    || record.engram.project_reset_in_progress || record.engram.bind_in_progress
                    || record.engram.checkpoint_in_progress || record.engram.checkpoint_status_owner.is_some()
                    || record.engram.admission_in_progress.is_some() || record.engram.pending_dispatch.is_some()
                    || record.engram.uncertain_grant_id.is_some()
                    || engram_project_for_session_locked(inner, &record.session.id)
                        .is_none_or(|project| project.id != operation.protection.project_id)
                    || record.engram.routing_token.as_ref() != Some(&operation.protection.routing_token)
                    || !(record.engram.active_grant_id.as_ref() == Some(&operation.protection.grant_id)
                        || accepted && record.engram.active_grant_id.is_none())
                    || record.engram.work_binding != operation.original.work_binding && !loaded_binding
                    || record.engram.checkpoint_operation.as_ref()
                        .is_none_or(|mirror| serde_json::to_value(mirror).ok() != serde_json::to_value(operation).ok())
                    || record.queued_prompts.iter().any(|head| head.has_engram_intent()
                        && !(record.queued_prompts.front().is_some_and(|first| first.pending_prompt.id == head.pending_prompt.id)
                            && checkpoint_recovery_original_head(record, operation)))
                {
                    return None;
                }
                Some(checkpoint_recovery_session_image(record))
            }
        };
        Some(Self { operation: operation.clone(), session })
    }

    fn matches(&self, inner: &StateInner) -> bool {
        if !checkpoint_recovery_project_matches(inner, &self.operation) { return false }
        let current = inner.engram_session_obligations.checkpoint_operations.iter()
            .find(|operation| operation.original.id == self.operation.original.id);
        if current.is_none_or(|operation| {
            checkpoint_receipt_identity(operation).ok() != checkpoint_receipt_identity(&self.operation).ok()
        }) { return false }
        let current_session = inner.find_session_index(&self.operation.protection.local_session_id)
            .map(|index| checkpoint_recovery_session_image(&inner.sessions[index]));
        current_session == self.session
    }

    fn fence(&self, inner: &StateInner) -> Result<EngramProjectContainmentFence> {
        let ids = self.session.as_ref().map(|_| self.operation.protection.local_session_id.clone())
            .into_iter().collect::<Vec<_>>();
        let mut image = EngramProjectContainmentFence::capture(inner, &self.operation.protection.project_id, &ids)?;
        if self.session.is_none() {
            if inner.find_session_index(&self.operation.protection.local_session_id).is_some() {
                bail!("Orphan checkpoint acquired a session row")
            }
            image.absent_sessions.push(self.operation.protection.local_session_id.clone());
        }
        if self.operation.protection.phase == EngramSessionObligationPhase::Confirmed {
            image.capture_checkpoint_effects(inner, &self.operation, self.session.is_some())?;
        }
        Ok(image)
    }
}

fn publish_checkpoint_recovery_operation(inner: &mut StateInner, operation: EngramCheckpointOperation) -> Result<()> {
    let mut ledger = inner.engram_session_obligations.clone();
    let slot = ledger.checkpoint_operations.iter_mut()
        .find(|current| current.original.id == operation.original.id)
        .ok_or_else(|| anyhow!("Original checkpoint duty disappeared"))?;
    *slot = operation.clone();
    ledger.validate()?;
    if let Some(index) = inner.find_session_index(&operation.protection.local_session_id) {
        let record = inner.session_mut_by_index(index).expect("selected checkpoint row");
        if record.engram.checkpoint_operation.as_ref().is_none_or(|mirror| mirror.original.id != operation.original.id) {
            bail!("Original checkpoint mirror changed")
        }
        record.engram.checkpoint_operation = Some(operation);
    }
    inner.engram_session_obligations = ledger;
    Ok(())
}

fn checkpoint_recovery_accounting_image(inner: &StateInner, operation: &EngramCheckpointOperation)
    -> Result<(Value, Option<Value>, EngramCheckpointEffectsFence)> {
    let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
        .find(|current| current.original.id == operation.original.id)
        .ok_or_else(|| anyhow!("Original checkpoint disappeared"))?;
    let mut fence = EngramProjectContainmentFence::capture(inner, &operation.protection.project_id, &[])?;
    fence.capture_checkpoint_effects(inner, operation, true)?;
    Ok((serde_json::to_value(canonical)?,
        inner.find_session_index(&operation.protection.local_session_id)
            .map(|index| checkpoint_recovery_session_image(&inner.sessions[index])),
        fence.checkpoint_effects.expect("captured effects")))
}

impl AppState {
    fn recover_original_checkpoint_before_admission(&self, session_id: &str,
        trigger: Option<&EngramQueuedAdmissionOwner>) -> Result<bool> {
        let id = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else { return Ok(false) };
            let record = &inner.sessions[index];
            if trigger.is_some_and(|trigger| !trigger.matches(record)) {
                bail!("Original checkpoint recovery trigger changed")
            }
            record.engram.checkpoint_operation.as_ref()
                .filter(|operation| EngramCheckpointResultOwner::capture(&inner, &operation.original.id).is_some())
                .map(|operation| operation.original.id.clone())
        };
        let Some(id) = id else { return Ok(false) };
        // The first admission-triggered recovery may run immediately. Once an
        // original has scheduling history, repeated Send/Resume and automatic
        // admission retries share its due time instead of renewing fast grace.
        let now = self.engram_budget_clock().wall_now();
        if self.engram_retry_slots.checkpoint_due.lock().expect("checkpoint retry schedule poisoned")
            .get(&id).is_some_and(|entry| entry.next_at > now) {
            bail!("Original checkpoint recovery awaits its scheduled retry")
        }
        let _slot = self.engram_retry_slots.try_acquire_opportunistic()
            .ok_or_else(|| anyhow!("Original checkpoint recovery awaits a shared retry slot"))?;
        let settled = self.recover_original_checkpoint_result(&id)?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        require_no_engram_session_obligation(&inner, session_id)
            .map_err(|error| anyhow!("{}", error.message))?;
        if trigger.is_some_and(|trigger| inner.find_session_index(session_id)
            .is_none_or(|index| !trigger.matches(&inner.sessions[index]))) {
            bail!("Checkpoint settled; admission trigger must be recaptured")
        }
        Ok(settled)
    }

    // The durable lineage selects fast versus slow cadence across restart.
    // The process-local due map adds no new debt ledger or network allowance.
    fn engram_checkpoint_result_retry_tick(&self, now: chrono::DateTime<chrono::Utc>) {
        if self.engram_retry_slots.boot_preparation_pending() { return }
        let (eligible, retained) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let eligible = inner.engram_session_obligations.checkpoint_operations.iter()
                .filter(|operation| EngramCheckpointResultOwner::capture(&inner, &operation.original.id).is_some())
                .map(|operation| (operation.original.id.clone(),
                    operation.attempts.last().expect("validated operation").id.clone(),
                    operation.attempts.len(), matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. })))
                .collect::<Vec<_>>();
            // Temporary owner/settings ineligibility does not renew grace.
            // Keep only entries backed by an unresolved canonical operation.
            let retained = inner.engram_session_obligations.checkpoint_operations.iter()
                .filter(|operation| operation.protection.phase != EngramSessionObligationPhase::Confirmed
                    || !inner.engram_confirmed_containment_ids.contains(&operation.original.id))
                .map(|operation| operation.original.id.clone()).collect::<HashSet<_>>();
            (eligible, retained)
        };
        let mut due = Vec::new();
        {
            let mut schedule = self.engram_retry_slots.checkpoint_due.lock().expect("checkpoint retry schedule poisoned");
            schedule.retain(|id, _| retained.contains(id));
            for (id, attempt, count, accepted) in eligible {
                let entry = schedule.entry(id.clone()).or_insert_with(||
                    EngramCheckpointRetryState::from_parts(&id, attempt.clone(), count, accepted, now));
                entry.observe(&id, &attempt, count, accepted, now);
                if entry.next_at <= now { due.push((entry.next_at, id)); }
            }
        }
        due.sort();
        // At most one bounded checkpoint call per tick; ordinary admission
        // retries run first. Failed attempts remain due on the normal cadence.
        if let Some((_, id)) = due.first()
            && let Some(_slot) = self.engram_retry_slots.try_acquire() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
                self.recover_original_checkpoint_result_at(id, now)));
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => eprintln!("engram> checkpoint original-result recovery remains held: {error:#}"),
                Err(_) => eprintln!("engram> checkpoint original-result recovery panicked; original duty retained"),
            }
        }
    }

    // One bounded result-recovery action. No generic admission guard is relaxed.
    // The current process owns the reservation; the linked durable attempt names
    // its actual current runtime/queue, never a resurrected old runtime token.
    fn recover_original_checkpoint_result(&self, operation_id: &str) -> Result<bool> {
        self.recover_original_checkpoint_result_with_proof(operation_id).map(|(released, _)| released)
    }

    fn recover_original_checkpoint_result_with_proof(&self, operation_id: &str) -> Result<(bool, bool)> {
        self.recover_original_checkpoint_result_at(operation_id, self.engram_budget_clock().wall_now())
    }

    fn recover_original_checkpoint_result_at(&self, operation_id: &str,
        now: chrono::DateTime<chrono::Utc>) -> Result<(bool, bool)> {
        let (pass, before_attempt, before_count) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(operation) = inner.engram_session_obligations.checkpoint_operations.iter()
                .find(|operation| operation.original.id == operation_id) else { return Ok((false, false)) };
            let attempt = operation.attempts.last().ok_or_else(|| anyhow!("Checkpoint has no attempt"))?.id.clone();
            let mut schedule = self.engram_retry_slots.checkpoint_due.lock().expect("checkpoint retry schedule poisoned");
            let entry = schedule.entry(operation_id.to_owned()).or_insert_with(|| EngramCheckpointRetryState::new(operation, now));
            entry.observe(operation_id, &attempt, operation.attempts.len(),
                matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. }), now);
            let pass = Uuid::new_v4();
            entry.pass = Some(pass);
            (pass, attempt, operation.attempts.len())
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
            self.recover_original_checkpoint_result_once(operation_id)));
        let failed = !matches!(&result, Ok(Ok(_)));
        let proved_held = matches!(&result, Ok(Ok((false, true))));
        {
            // Apply completion only to this original and this process-local
            // pass. An older failure cannot overwrite a newer Accepted proof.
            let inner = self.inner.lock().expect("state mutex poisoned");
            let mut schedule = self.engram_retry_slots.checkpoint_due.lock().expect("checkpoint retry schedule poisoned");
            if let Some(operation) = inner.engram_session_obligations.checkpoint_operations.iter()
                .find(|operation| operation.original.id == operation_id) {
                if operation.protection.phase == EngramSessionObligationPhase::Confirmed
                    && inner.engram_confirmed_containment_ids.contains(operation_id) {
                    schedule.remove(operation_id);
                } else if let Some(entry) = schedule.get_mut(operation_id)
                    && entry.pass == Some(pass) {
                    let attempt = &operation.attempts.last().expect("retained operation").id;
                    let accepted = matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. });
                    entry.observe(operation_id, attempt, operation.attempts.len(), accepted, now);
                    if !accepted && failed && attempt == &before_attempt
                        && operation.attempts.len() == before_count {
                        entry.preallocation_failures = entry.preallocation_failures.saturating_add(1)
                            .min(ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT);
                    }
                    let delay = if accepted {
                        // Only the actual writer proof can choose held/slow.
                        if proved_held { checkpoint_result_retry_delay(operation_id, ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT) }
                        else { engram_abort_retry_delay(operation_id, 1) }
                    } else { checkpoint_result_retry_delay(operation_id,
                        operation.attempts.len().max(entry.preallocation_failures)) };
                    entry.next_at = now + delay;
                    entry.pass = None;
                }
            } else { schedule.remove(operation_id); }
        }
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    // Second result component records an actual successful writer proof.
    fn recover_original_checkpoint_result_once(&self, operation_id: &str) -> Result<(bool, bool)> {
        let (candidate, adapter, clock) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(owner) = EngramCheckpointResultOwner::capture(&inner, operation_id) else { return Ok((false, false)) };
            (owner, inner.engram_host_adapter.clone(), inner.engram_budget_clock_snapshot())
        };
        let session_id = candidate.operation.protection.local_session_id.clone();
        let started = self.checkpoint_preflight_budget(&session_id).map_or_else(|| clock.now(), |budget| budget.0);
        let deadline = started + candidate.operation.original.settings.call_timeout();
        if !matches!(candidate.operation.progress, EngramExistingCheckpointProgress::Accepted { .. }) {
            if candidate.operation.attempts.len() >= ENGRAM_CHECKPOINT_ATTEMPT_CAPACITY {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                if candidate.matches(&inner) {
                    let mut held = candidate.operation.clone();
                    held.protection.reason_code = "original_checkpoint_attempt_capacity".into();
                    publish_checkpoint_recovery_operation(&mut inner, held)?;
                    self.commit_locked(&mut inner)?;
                }
                bail!("Original checkpoint attempt capacity exhausted; original custody and exclusion retained")
            }
            // A failed read-only probe does not allocate or abandon a send
            // attempt. The session's current claim is not read: the producer
            // settles the original against its stored control-session
            // binding, so a claim since released, re-fenced or replaced never
            // withholds the exact original request.
            adapter.read_observation_policy(&candidate.operation.original.connection,
                &candidate.operation.protection.authority,
                deadline.saturating_duration_since(clock.now()))
                .map_err(|error| anyhow!("{error}"))?;
        }
        let (mut owner, reservation) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !candidate.matches(&inner) { bail!("Original checkpoint recovery owner changed during preflight") }
            let Some(mut owner) = EngramCheckpointResultOwner::capture(&inner, operation_id) else { return Ok((false, false)) };
            let accepted = matches!(owner.operation.progress, EngramExistingCheckpointProgress::Accepted { .. });
            if !accepted {
                if owner.operation.attempts.len() >= ENGRAM_CHECKPOINT_ATTEMPT_CAPACITY { bail!("Checkpoint attempt capacity exhausted") }
                let record = inner.find_session_index(&owner.operation.protection.local_session_id)
                    .map(|index| &inner.sessions[index]);
                owner.operation.attempts.push(EngramCheckpointAttempt {
                    id: Uuid::new_v4().to_string(),
                    predecessor: owner.operation.attempts.last().map(|attempt| attempt.id.clone()),
                    dispatch_generation: record.map_or(owner.operation.original.dispatch_generation, |record| record.engram.dispatch_generation),
                    active_turn_generation: record.map_or(0, |record| record.active_turn_generation),
                    runtime_episode: record.map_or_else(|| format!("recovery:{}", Uuid::new_v4()), |record| format!("{:?}", record.runtime.runtime_token())),
                    queued_prompt_id: record.and_then(|record| record.queued_prompts.front()).map(|head| head.pending_prompt.id.clone()),
                    queued_fingerprint: record.and_then(|record| record.queued_prompts.front()).map(containment_prompt_fingerprint),
                    result: None,
                });
                owner.operation.progress = EngramExistingCheckpointProgress::Retained;
                owner.operation.protection.phase = EngramSessionObligationPhase::Intended;
                owner.operation.protection.settlement_evidence = Value::Null;
            }
            let key = checkpoint_receipt_key(&owner.operation);
            let identity = checkpoint_receipt_identity(&owner.operation)?;
            if let Some(pending) = inner.engram_pending_checkpoint_receipts.entries.get_mut(&key) {
                if !accepted || pending.origin == EngramCheckpointReceiptOrigin::OffContainment
                    || pending.accounting || !pending.accepted || pending.identity != identity
                    || !matches!(&owner.operation.progress, EngramExistingCheckpointProgress::Accepted { response }
                        if pending.response.as_deref().is_some_and(|bytes| serde_json::from_slice::<Value>(bytes)
                            .is_ok_and(|value| &value == response))) {
                    bail!("Original checkpoint recovery already has an owner or pending receipt")
                }
                pending.origin = EngramCheckpointReceiptOrigin::OriginalRecovery;
                pending.accounting = true;
            } else {
                inner.engram_pending_checkpoint_receipts.reserve_identity(key.clone(), identity,
                    EngramCheckpointReceiptOrigin::OriginalRecovery)?;
            }
            // Construct the guard only after leaving the lock. Commit failure
            // explicitly returns the reservation to the bounded pool here.
            if !accepted && let Err(error) = publish_checkpoint_recovery_operation(&mut inner, owner.operation.clone())
                .and_then(|()| self.commit_locked(&mut inner).map(|_| ())) {
                if let Some(pending) = inner.engram_pending_checkpoint_receipts.entries.get_mut(&key)
                    && pending.accepted {
                    pending.accounting = false;
                } else {
                    inner.engram_pending_checkpoint_receipts.remove(&key);
                }
                return Err(error);
            }
            (owner, EngramCheckpointReceiptReservation { state: self.clone(), key })
        };
        let response = match &owner.operation.progress {
            EngramExistingCheckpointProgress::Accepted { response } => response.clone(),
            _ => {
                for possibly_sent in [false, true] {
                    let image = {
                        let mut inner = self.inner.lock().expect("state mutex poisoned");
                        if !owner.matches(&inner) { bail!("Original checkpoint recovery owner changed before send") }
                        if possibly_sent {
                            owner.operation.progress = EngramExistingCheckpointProgress::PossiblySent;
                            owner.operation.protection.phase = EngramSessionObligationPhase::Sent;
                            publish_checkpoint_recovery_operation(&mut inner, owner.operation.clone())?;
                            self.commit_locked(&mut inner)?;
                        }
                        owner.fence(&inner)?
                    };
                    self.confirm_engram_containment_until(image, deadline)
                        .map_err(|error| anyhow!("{}", error.message))?;
                    if !owner.matches(&self.inner.lock().expect("state mutex poisoned")) {
                        bail!("Original checkpoint recovery owner changed during acknowledgement")
                    }
                }
                let timeout = deadline.saturating_duration_since(clock.now());
                if timeout.is_zero() { bail!("Original checkpoint recovery deadline expired before send") }
                match adapter.request(&owner.operation.original.connection, &owner.operation.original.request, timeout) {
                    Ok(response) => response,
                    Err(error) => {
                        let result = if error.invocation_application() == EngramInvocationApplication::CertifiedNotApplied {
                            EngramCheckpointAttemptResult::CertifiedNotApplied { code: error.code.clone().expect("qualified producer code") }
                        } else { EngramCheckpointAttemptResult::Unknown { code: error.code.clone() } };
                        self.retain_checkpoint_recovery_result(&owner, None, result)?;
                        return Err(anyhow!("Original checkpoint result remains unresolved: {error}"));
                    }
                }
            }
        };
        let bytes = bounded_checkpoint_json(&response, ENGRAM_CHECKPOINT_REPLY_MAX_BYTES)?;
        let parsed = parse_engram_result::<EngramTurnCheckpointResponse>(response.clone())
            .map_err(|error| anyhow!("{error}"))?;
        let accepted = matches!(&parsed, EngramTurnCheckpointResponse::Checkpointed { receipt }
            if receipt.grant_id == owner.operation.protection.grant_id);
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            inner.engram_pending_checkpoint_receipts.receive_identity(&reservation.key,
                &checkpoint_receipt_identity(&owner.operation)?, bytes, accepted)?;
        }
        match parsed {
            EngramTurnCheckpointResponse::Refuse { code } => {
                self.retain_checkpoint_recovery_result(&owner, None,
                    EngramCheckpointAttemptResult::Refused { response: checkpoint_refusal_projection(&code)? })?;
                Ok((false, false))
            }
            EngramTurnCheckpointResponse::Checkpointed { .. } if !accepted => {
                self.retain_checkpoint_recovery_result(&owner, None,
                    EngramCheckpointAttemptResult::Unknown { code: Some("checkpoint_receipt_mismatch".into()) })?;
                bail!("Recovered checkpoint receipt does not name the original grant")
            }
            EngramTurnCheckpointResponse::Checkpointed { .. } => {
                let Some(mut accounted_owner) = self.retain_checkpoint_recovery_result(
                    &owner, Some(&response), EngramCheckpointAttemptResult::Accepted,
                )? else { bail!("Recovered checkpoint retained without the current accounting owner") };
                let released = self.acknowledge_checkpoint_result_recovery(&mut accounted_owner, deadline)?;
                self.inner.lock().expect("state mutex poisoned")
                    .engram_pending_checkpoint_receipts.remove(&reservation.key);
                Ok((released, true))
            }
        }
    }

    // The same canonical merge that retains ordinary late replies also retains
    // recovery replies after a current owner drifts. Only a matching current
    // recovery owner may additionally clear its old grant/head and confirm it.
    fn retain_checkpoint_recovery_result(&self, owner: &EngramCheckpointResultOwner,
        response: Option<&Value>, result: EngramCheckpointAttemptResult) -> Result<Option<EngramCheckpointResultOwner>> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let owns = owner.matches(&inner);
        let before = checkpoint_recovery_accounting_image(&inner, &owner.operation)?;
        let previously_accounted = owner.operation.protection.phase == EngramSessionObligationPhase::Confirmed
            && matches!(owner.operation.progress, EngramExistingCheckpointProgress::Accepted { .. });
        let (mut ledger, _, mut canonical) = prospective_checkpoint_result_with_live_owner(
            &inner, &owner.operation.protection.local_session_id, &owner.operation, response, Some(&result), None, false)?;
        if owns && response.is_some() {
            canonical.protection.phase = EngramSessionObligationPhase::Confirmed;
        }
        let code = match &result {
            EngramCheckpointAttemptResult::Refused { .. } => "original_checkpoint_refused",
            EngramCheckpointAttemptResult::Accepted => "original_checkpoint_accounting_pending",
            _ => "original_checkpoint_unknown",
        };
        canonical.protection.reason_code = code.into();
        *ledger.checkpoint_operations.iter_mut().find(|operation| operation.original.id == canonical.original.id)
            .expect("merged canonical") = canonical.clone();
        // Validate the complete prospective ledger before mutating any row.
        ledger.validate()?;
        if owns && response.is_some()
            && let Some(index) = inner.find_session_index(&owner.operation.protection.local_session_id)
            && checkpoint_recovery_original_head(&inner.sessions[index], &owner.operation)
            && inner.sessions[index].engram.dispatch_generation.checked_add(1).is_none() {
            bail!("Checkpoint recovery generation exhausted")
        }
        let mut settled_observations = Vec::new();
        if owns && response.is_some() {
            let session_id = &owner.operation.protection.local_session_id;
            let EngramControlRequest::TurnCheckpoint { grant_id, next_intent, .. } = &owner.operation.original.request
                else { bail!("Not a checkpoint") };
            settled_observations = Self::settle_checkpoint_receipt_effects_locked(
                &mut inner, &owner.operation, response.expect("owned response"), true,
            )?;
            if !previously_accounted && let Some(index) = inner.find_session_index(session_id) {
                let retire = checkpoint_recovery_original_head(&inner.sessions[index], &owner.operation);
                let record = inner.session_mut_by_index(index).expect("current recovery row");
                if retire {
                    record.queued_prompts.pop_front();
                    record.engram.dispatch_generation += 1;
                    sync_pending_prompts(record);
                }
                record.engram.active_grant_id = None;
                record.engram.retained_checkpoint = None;
                advance_engram_continuity_anchor(record, session_id, grant_id);
                if *next_intent == EngramNextIntent::Exit { record.engram.rebind_required = true; }
            }
        }
        // Only the original component is confirmed. Adopted enclosing duties
        // retain their independent phase and release guards.
        inner.engram_session_obligations = ledger;
        if let Some(index) = inner.find_session_index(&canonical.protection.local_session_id)
            && inner.sessions[index].engram.checkpoint_operation.as_ref()
                .is_some_and(|mirror| mirror.original.id == canonical.original.id
                    && serde_json::to_value(mirror).ok() != serde_json::to_value(&canonical).ok()) {
            inner.session_mut_by_index(index).expect("retained mirror").engram.checkpoint_operation = Some(canonical.clone());
        }
        let changed = before != checkpoint_recovery_accounting_image(&inner, &canonical)?;
        if changed {
            let committed = self.commit_locked(&mut inner);
            // Keep failed first publication visible as failed/pending evidence
            // too; retry can then publish these same in-memory effect rows.
            self.persist_test_run_evidence_batch_deferring_compaction_locked(&mut inner, settled_observations, Vec::new());
            committed?;
        } else {
            settled_observations.retain(|row|
                !inner.test_runs.observations_durable.contains(&row.execution_id)
                    || inner.test_runs.observations_failed.contains(&row.execution_id));
            if !settled_observations.is_empty() {
                self.persist_test_run_evidence_batch_deferring_compaction_locked(&mut inner, settled_observations, Vec::new());
            }
        }
        Ok((owns && response.is_some()).then(|| EngramCheckpointResultOwner {
            session: inner.find_session_index(&canonical.protection.local_session_id)
                .map(|index| checkpoint_recovery_session_image(&inner.sessions[index])),
            operation: canonical,
        }))
    }

    fn acknowledge_checkpoint_result_recovery(&self, owner: &mut EngramCheckpointResultOwner,
        deadline: std::time::Instant) -> Result<bool> {
        let (image, session) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !owner.matches(&inner) {
                bail!("Recovered checkpoint accounting owner changed before acknowledgement")
            }
            let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
                .find(|operation| operation.original.id == owner.operation.original.id)
                .ok_or_else(|| anyhow!("Recovered checkpoint duty disappeared"))?;
            if checkpoint_receipt_identity(canonical)? != checkpoint_receipt_identity(&owner.operation)?
                || canonical.protection.phase != EngramSessionObligationPhase::Confirmed
                || !matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. }) {
                bail!("Recovered receipt retained but current accounting owner changed")
            }
            owner.operation = canonical.clone();
            let session = inner.find_session_index(&canonical.protection.local_session_id)
                .map(|index| checkpoint_recovery_session_image(&inner.sessions[index]));
            (owner.fence(&inner)?, session)
        };
        self.confirm_engram_containment_until(image.clone(), deadline)
            .map_err(|error| anyhow!("{}", error.message))?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let current = owner.fence(&inner)?;
        let current_session = inner.find_session_index(&owner.operation.protection.local_session_id)
            .map(|index| checkpoint_recovery_session_image(&inner.sessions[index]));
        if current.obligations != image.obligations || current.sessions != image.sessions
            || current.settings != image.settings || current_session != session
            || current.checkpoint_effects != image.checkpoint_effects {
            bail!("Recovered checkpoint acknowledgement owner changed")
        }
        // An adopted enclosing duty shares the original checkpoint's ID. Its
        // independent custody is not settled by this component's receipt.
        let selected_is_confirmed = inner.engram_session_obligations.all_protections()
            .find(|entry| entry.id == owner.operation.original.id)
            .is_some_and(|entry| engram_containment_confirmation_materialized(&inner, entry));
        if selected_is_confirmed
            && inner.engram_confirmed_containment_ids.insert(owner.operation.original.id.clone()) {
            // Publish only a changed latch. Held component accounting needs
            // no additional metadata transaction or state broadcast.
            self.commit_locked(&mut inner)?;
        }
        Ok(selected_is_confirmed)
    }
}
