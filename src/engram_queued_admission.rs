// Durable wire intent for the existing queued prompt, not a second queue.
// Owns exact authorization replay and its original store/principal association.
// Does not infer provider non-delivery from an Engram begin receipt.

fn engram_admission_persisted_content(record: &PersistedSessionRecord) -> Value {
    json!({ "generation": record.engram_dispatch_generation,
        "routing": record.engram_routing_token, "grant": record.engram_open_grant_id,
        "queue": record.queued_prompts.front() })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EngramQueuedBind {
    connection: EngramConnectionConfig,
    settings: EngramProjectSettings,
    request: EngramControlRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation_generation: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EngramQueuedEvaluate {
    connection: EngramConnectionConfig,
    settings: EngramProjectSettings,
    request: EngramControlRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation_generation: Option<u64>,
    // A durable begin acknowledgment is a possibly-delivered marker even after
    // the remote grant closes. Never infer non-delivery from session_status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    begun_grant_id: Option<String>,
}

impl EngramProjectSettings {
    fn same_admission_settings(&self, other: &Self) -> bool {
        let mut left = self.clone();
        let mut right = other.clone();
        left.acceptance_evaluation = None;
        right.acceptance_evaluation = None;
        left == right
    }
}

impl QueuedPromptRecord {
    fn has_engram_intent(&self) -> bool {
        self.engram_bind.is_some() || self.engram_evaluate.is_some()
    }

    // Stop can retain the head before the first wire request is prepared.
    // Ordering and coalescing must protect that head just like saved intent.
    fn is_engram_retained(&self) -> bool {
        self.has_engram_intent() || self.engram_interrupted || self.engram_waiting
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EngramAdmissionDisposition {
    Retry,
    Reconcile,
    Reject,
    Ready,
}

fn engram_admission_disposition(card: &EngramControlCard) -> EngramAdmissionDisposition {
    match card.decision {
        EngramControlCardDecision::Defer => EngramAdmissionDisposition::Retry,
        EngramControlCardDecision::Refuse => EngramAdmissionDisposition::Reject,
        EngramControlCardDecision::Grant => EngramAdmissionDisposition::Ready,
        EngramControlCardDecision::Degraded => match card.refusal_code.as_deref() {
            Some("control_disabled") => EngramAdmissionDisposition::Reject,
            Some(
                "deadline_exceeded"
                | "control_unavailable"
                | "control_circuit_open"
                | "dispatch_budget_exhausted"
                | "control_backoff",
            ) => EngramAdmissionDisposition::Retry,
            // Protocol/store faults and local persistence/ownership failures
            // do not establish non-delivery. Retain but never automatically
            // replay them; unknown future codes get the same safe disposition.
            _ => EngramAdmissionDisposition::Reconcile,
        },
    }
}

// A Stop owner may roll back after admission has already consumed pending_dispatch.
// Keep the exact prepared delivery in its existing callback queue. Cloned callback
// lists share a one-shot payload, so rollback replay cannot enqueue it twice.
#[derive(Clone)]
struct DeferredEngramHandoff(Arc<Mutex<Option<TurnDispatch>>>);

impl std::fmt::Debug for DeferredEngramHandoff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeferredEngramHandoff")
    }
}

impl PartialEq for DeferredEngramHandoff {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for DeferredEngramHandoff {}

// Identity of the queue head that owns an off-lock recovery operation.
#[derive(Clone)]
struct EngramQueuedAdmissionOwner {
    prompt_id: String,
    generation: u64,
    active_turn_generation: u64,
}

impl EngramQueuedAdmissionOwner {
    fn capture_promoted(record: &SessionRecord) -> Option<Self> {
        let owner = Self::capture(record)?;
        record
            .queued_prompts
            .front()?
            .promoted_message_index
            .map(|_| owner)
    }

    fn capture(record: &SessionRecord) -> Option<Self> {
        record.queued_prompts.front().map(|queued| Self {
            prompt_id: queued.pending_prompt.id.clone(),
            generation: record.engram.dispatch_generation,
            active_turn_generation: record.active_turn_generation,
        })
    }

    fn matches(&self, record: &SessionRecord) -> bool {
        record.engram.dispatch_generation == self.generation
            && record.active_turn_generation == self.active_turn_generation
            && record
                .queued_prompts
                .front()
                .is_some_and(|queued| queued.pending_prompt.id == self.prompt_id)
    }
}

fn legacy_queued_engram_operation_generation(
    session_id: &str,
    idempotency_key: &str,
) -> Option<u64> {
    [
        "termal-evaluate:",
        "termal-stale-reevaluate:",
        "termal-reevaluate:",
    ]
    .into_iter()
    .find_map(|host_prefix| {
        let prefix = format!("{host_prefix}{session_id}:");
        let remainder = idempotency_key.strip_prefix(&prefix)?;
        let (generation, opaque_suffix) = remainder.split_once(':')?;
        (!opaque_suffix.is_empty())
            .then(|| generation.parse::<u64>().ok())
            .flatten()
    })
}

// Ephemeral single-flight ownership. A competing drain leaves a wake on this
// exact attempt; reset/reconfiguration cannot let an old guard clear a new one.
struct EngramAdmissionGuard<'a> {
    state: &'a AppState,
    session_id: &'a str,
    wake: Arc<std::sync::atomic::AtomicBool>,
    owner: Option<EngramQueuedAdmissionOwner>,
}

impl EngramAdmissionGuard<'_> {
    fn release(&self) -> bool {
        let Ok(mut inner) = self.state.inner.lock() else {
            return false;
        };
        let Some(index) = inner.find_session_index(self.session_id) else {
            return false;
        };
        let record = &mut inner.sessions[index];
        if !record
            .engram
            .admission_in_progress
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, &self.wake))
        {
            return false;
        }
        record.engram.admission_in_progress = None;
        if !self
            .owner
            .as_ref()
            .is_some_and(|owner| owner.matches(record))
        {
            return false;
        }
        self.wake.load(std::sync::atomic::Ordering::Relaxed)
            && matches!(
                record.session.status,
                SessionStatus::Idle | SessionStatus::Error
            )
            && !record.orchestrator_auto_dispatch_blocked
            && record
                .queued_prompts
                .front()
                .is_some_and(|queued| !queued.engram_interrupted)
    }
}

impl Drop for EngramAdmissionGuard<'_> {
    fn drop(&mut self) {
        // Unwind releases ownership without attempting provider work.
        self.release();
    }
}

// Stop/terminalization of a promoted turn must not leave that same turn as a
// runnable successor. Unknown admission is parked separately, before retry.
/// Whether the queued head is the promoted prompt of the dispatch carrying
/// `fingerprint`: the one [`retire_promoted_engram_head`] removes.
fn promoted_engram_head_matches(record: &SessionRecord, fingerprint: &str) -> bool {
    record.queued_prompts.front().is_some_and(|queued| {
        queued.promoted_message_index.is_some()
            && engram_turn_intent_fingerprint(
                &queued.pending_prompt.text,
                queued.pending_prompt.expanded_text.as_deref(),
                &queued.attachments,
                queued.pending_prompt.source.as_ref(),
                queued.source,
            ) == fingerprint
    })
}

fn retire_promoted_engram_head(record: &mut SessionRecord, fingerprint: &str) {
    if promoted_engram_head_matches(record, fingerprint) {
        record.queued_prompts.pop_front();
        sync_pending_prompts(record);
    }
}

impl AppState {
    fn queued_engram_owner_is_current(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
    ) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .is_some_and(|index| owner.matches(&inner.sessions[index]))
    }

    /// Proves the owner still holds the prompt and, under that same lock,
    /// records which grant its dispatch is handing to `turn_begin`, so an
    /// abandon racing the in-flight request mirrors exactly that grant as
    /// possibly begun instead of assuming Engram never saw it. A begin whose
    /// uncertainty cannot be recorded, because the dispatch is no longer
    /// pending, is not authorized.
    fn mark_engram_begin_requested_if_current(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
        dispatch_generation: u64,
        grant_id: &str,
    ) -> bool {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return false;
        };
        if !owner.matches(&inner.sessions[index]) {
            return false;
        }
        match inner.sessions[index].engram.pending_dispatch.as_mut() {
            Some(pending) if pending.dispatch_generation == dispatch_generation => {
                pending.begin_requested = Some(grant_id.to_owned());
                true
            }
            _ => false,
        }
    }

    /// Forgets the recorded begin once Engram has definitively refused it, so
    /// an abandon that races the re-evaluation, or the gap before the record
    /// is finished, does not record a grant Engram never began as uncertain.
    fn clear_engram_begin_requested_if_current(&self, session_id: &str, dispatch_generation: u64) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        if let Some(pending) = inner.sessions[index].engram.pending_dispatch.as_mut()
            && pending.dispatch_generation == dispatch_generation
        {
            pending.begin_requested = None;
        }
    }

    fn require_queued_engram_owner(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
        operation: &str,
    ) -> std::result::Result<(), EngramTransportError> {
        self.queued_engram_owner_is_current(session_id, owner)
            .then_some(())
            .ok_or_else(|| {
                EngramTransportError::local_state(format!(
                    "{operation} no longer owns the queued prompt"
                ))
            })
    }

    fn engram_issued_grant_was_retired(
        &self,
        target: &EngramBindingTarget,
        started_at: std::time::Instant,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> bool {
        let Some(timeout) = target.remaining_dispatch_timeout(started_at) else {
            return false;
        };
        let Some(routing_token) = target.routing_token.as_ref() else {
            return false;
        };
        if !owner.is_some_and(|owner| {
            self.queued_engram_owner_is_current(&target.connection.session_id, owner)
        }) {
            return false;
        }
        let response = target
            .adapter
            .request(
                &target.connection,
                &EngramControlRequest::SessionStatus {
                    routing_token: routing_token.clone(),
                },
                timeout,
            )
            .and_then(parse_engram_result::<EngramSessionStatusResponse>);
        if !owner.is_some_and(|owner| {
            self.queued_engram_owner_is_current(&target.connection.session_id, owner)
        }) {
            return false;
        }
        matches!(response,
            Ok(status) if status.open_grant_id.is_none() && status.phase == "sync_required")
    }

    fn confirm_engram_admission_durable(
        &self,
        session_id: &str,
        started_at: std::time::Instant,
        owner: &EngramQueuedAdmissionOwner,
    ) -> std::result::Result<(), EngramTransportError> {
        let target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(session_id).ok_or_else(|| {
                EngramTransportError::local_state("Session removed before durable admission")
            })?;
            let record = &inner.sessions[index];
            if !owner.matches(record) {
                return Err(EngramTransportError::local_state(
                    "Admission durability fence no longer owns the queued prompt",
                ));
            }
            PersistFenceTarget::EngramAdmission {
                session_id: session_id.to_owned(),
                content: json!({
                    "generation": record.engram.dispatch_generation, "routing": record.engram.routing_token,
                    "grant": record.engram.active_grant_id, "queue": record.queued_prompts.front(),
                }),
            }
        };
        let (fence, waiter) = PersistFence::new(
            target,
            started_at + Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS),
        );
        if self
            .persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok()
        {
            waiter.wait().map_err(|error| {
                EngramTransportError::deadline(format!(
                    "Engram admission durability is unknown: {error:?}"
                ))
            })?;
        } else {
            // Test/shutdown fallback: commit calls used synchronous persistence.
            // Recheck via the same persistence path rather than assume success.
            let inner = self.inner.lock().expect("state mutex poisoned");
            self.persist_internal_locked(&inner).map_err(|error| {
                EngramTransportError::local_state(format!(
                    "Engram admission persistence failed: {error:#}"
                ))
            })?;
        }
        self.require_queued_engram_owner(session_id, owner, "Admission durability fence")?;
        Ok(())
    }
    fn retire_queued_engram_evaluation(
        &self,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> std::result::Result<(), EngramTransportError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id) {
            let record = &inner.sessions[index];
            if !owner.map_or(record.queued_prompts.is_empty(), |owner| {
                owner.matches(record)
            }) {
                return Err(EngramTransportError::local_state(
                    "Evaluation retirement no longer owns the queued prompt",
                ));
            }
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            let retained_before = record
                .queued_prompts
                .front()
                .is_some_and(QueuedPromptRecord::is_engram_retained);
            if let Some(queued) = record.queued_prompts.front_mut() {
                queued.engram_evaluate = None;
            }
            sync_pending_prompts(record);
            let retention_changed = retained_before
                != record
                    .queued_prompts
                    .front()
                    .is_some_and(QueuedPromptRecord::is_engram_retained);
            if retention_changed {
                if let Err(error) = self.commit_locked(&mut inner) {
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    if let Some(queued) = record.queued_prompts.front_mut() {
                        queued.engram_interrupted = true;
                    }
                    record.set_auto_dispatch_blocked(true);
                    record.session.preview = "Engram evaluation retirement persistence is unknown. Prompt retained; reconcile before continuing.".to_owned();
                    sync_pending_prompts(record);
                    self.publish_state_locked(&inner);
                    return Err(EngramTransportError::local_state(format!(
                        "Failed to retire terminal evaluation: {error:#}"
                    )));
                }
            } else {
                self.persist_internal_locked(&inner).map_err(|error| {
                    EngramTransportError::local_state(format!(
                        "Failed to retire terminal evaluation: {error:#}"
                    ))
                })?;
            }
        }
        Ok(())
    }

    fn stop_waiting_engram_admission(
        &self,
        session_id: &str,
    ) -> std::result::Result<bool, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_visible_session_index(session_id) else {
            return Ok(false);
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if record
            .queued_prompts
            .front()
            .is_some_and(|queued| queued.engram_interrupted)
            || !matches!(
                record.session.status,
                SessionStatus::Idle | SessionStatus::Error
            )
            || !(record.engram.admission_in_progress.is_some()
                || record
                    .queued_prompts
                    .front()
                    .is_some_and(QueuedPromptRecord::has_engram_intent))
        {
            return Ok(false);
        }
        record.engram.dispatch_generation = record.engram.dispatch_generation.saturating_add(1);
        detach_engram_pending_dispatch_keeping_uncertain_begin(record);
        record.engram.rebind_required = true;
        if let Some(queued) = record.queued_prompts.front_mut() {
            queued.engram_interrupted = true;
        }
        record.set_auto_dispatch_blocked(true);
        record.session.preview = "Engram authorization canceled. Prompt retained; remove it before starting a new operation.".to_owned();
        record.session.live_activity = None;
        sync_pending_prompts(record);
        self.commit_locked(&mut inner).map_err(|error| {
            ApiError::internal(format!(
                "Failed to persist authorization cancellation: {error:#}"
            ))
        })?;
        Ok(true)
    }

    fn queued_engram_generation(&self, session_id: &str, prompt_id: &str, previous: u64) -> u64 {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let Some(queued) = inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions[index].queued_prompts.front())
            .filter(|queued| queued.pending_prompt.id == prompt_id)
        else {
            return previous.saturating_add(1);
        };
        if let Some(generation) = queued
            .engram_evaluate
            .as_ref()
            .and_then(|prepared| prepared.operation_generation)
            .or_else(|| {
                queued
                    .engram_bind
                    .as_ref()
                    .and_then(|prepared| prepared.operation_generation)
            })
        {
            return generation;
        }
        // Compatibility for prepared rows written before the explicit field:
        // recover the generation from TermAl's deterministic evaluate key.
        if let Some(generation) = queued.engram_evaluate.as_ref().and_then(|prepared| {
            let EngramControlRequest::TurnEvaluate {
                idempotency_key, ..
            } = &prepared.request
            else {
                return None;
            };
            legacy_queued_engram_operation_generation(session_id, idempotency_key)
        }) {
            return generation;
        }
        if queued.engram_bind.is_some() {
            return previous.saturating_add(1);
        }
        if queued.promoted_message_index.is_some() {
            previous
        } else {
            previous.saturating_add(1)
        }
    }

    fn park_unknown_engram_authorization(
        &self,
        session_id: &str,
        generation: u64,
        runtime_token: &RuntimeToken,
        active_turn_generation: u64,
    ) -> EngramAuthorizationParkOutcome {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return EngramAuthorizationParkOutcome::Superseded;
        };
        let park_is_current = {
            let record = &inner.sessions[index];
            record.runtime.matches_runtime_token(runtime_token)
                && record.active_turn_generation == active_turn_generation
                && !record.runtime_stop_in_progress
                && record.session.status == SessionStatus::Active
        };
        if !park_is_current {
            return EngramAuthorizationParkOutcome::Superseded;
        }
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let mut interrupted = record
            .queued_prompts
            .front()
            .is_some_and(|queued| queued.engram_interrupted);
        let decision = record
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::EngramControl { card, .. } => Some((
                    engram_admission_disposition(card),
                    card.decision == EngramControlCardDecision::Defer,
                )),
                _ => None,
            });
        interrupted |= record
            .queued_prompts
            .front()
            .is_some_and(QueuedPromptRecord::is_engram_retained)
            && decision.is_some_and(|(disposition, _)| {
                disposition == EngramAdmissionDisposition::Reconcile
            });
        let waiting = record.engram.dispatch_generation == generation
            && !record.queued_prompts.is_empty()
            && (interrupted
                || decision.is_some_and(|(disposition, _)| {
                    disposition == EngramAdmissionDisposition::Retry
                }));
        if !waiting {
            return EngramAuthorizationParkOutcome::Superseded;
        }
        if decision.is_some_and(|(_, defer)| defer) {
            record.engram.dispatch_generation = record.engram.dispatch_generation.saturating_add(1);
        }
        record.set_auto_dispatch_blocked(true);
        record.session.status = SessionStatus::Idle;
        if let Some(queued) = record.queued_prompts.front_mut() {
            // A known Defer has retired its evaluation and may have reused an
            // existing bind. It still owns a held prompt without wire intent.
            queued.engram_waiting = true;
            queued.engram_interrupted = interrupted;
        }
        record.session.preview =
            (if interrupted { "Engram: Waiting/Unknown after interrupted authorization. Prompt retained; cancel or reconcile before continuing." }
            else { "Engram: Waiting/Unknown. Original prompt retained; resume to retry or cancel." })
                .to_owned();
        record.session.live_activity = None;
        // No provider handoff occurred. Do not run terminal failure refresh:
        // it would fail the delegation and delete the very intent being held.
        clear_active_turn_file_change_tracking(record);
        sync_pending_prompts(record);
        if let Err(error) = self.commit_locked(&mut inner) {
            eprintln!("engram> failed persisting waiting authorization: {error:#}");
            // The state transition may or may not have reached the durable
            // store. Keep the exact retained head blocked in memory and make
            // that uncertainty visible without claiming the write committed.
            self.publish_state_locked(&inner);
            return EngramAuthorizationParkOutcome::PersistenceUnknown;
        }
        EngramAuthorizationParkOutcome::Parked
    }

    // Reuse the original token before considering a rebind. An evaluate reply
    // may have been lost after its grant was committed to the control store.
    fn restore_queued_engram_target(
        &self,
        target: &mut EngramBindingTarget,
        owner: &EngramQueuedAdmissionOwner,
    ) -> std::result::Result<bool, EngramTransportError> {
        let (prepared, bind, cold) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(&target.connection.session_id) else {
                return Ok(false);
            };
            let record = &inner.sessions[index];
            if !owner.matches(record) {
                return Err(EngramTransportError::local_state(
                    "Recovery no longer owns the queued prompt",
                ));
            }
            let prepared = record
                .queued_prompts
                .front()
                .and_then(|queued| queued.engram_evaluate.clone());
            let bind = record
                .queued_prompts
                .front()
                .and_then(|queued| queued.engram_bind.clone());
            (prepared, bind, record.engram.recovered_admission)
        };
        if let Some(bind) = bind {
            if bind.connection != target.connection
                || !bind.settings.same_admission_settings(&target.settings)
            {
                self.interrupt_queued_engram_admission(&target.connection.session_id, owner)?;
                return Err(EngramTransportError::local_state(
                    "Retained bind belongs to different settings; cancel or reconcile it first",
                ));
            }
            target.rebind_required = false;
            target.circuit_open = false;
            target.next_bind_retry_at = None;
            // Replay the retained bind itself, even when the last acknowledged
            // routing token is still present. Do not status/rebind that token.
            target.routing_token = None;
        }
        let Some(prepared) = prepared else {
            return Ok(false);
        };
        if prepared.connection != target.connection
            || !prepared.settings.same_admission_settings(&target.settings)
        {
            self.interrupt_queued_engram_admission(&target.connection.session_id, owner)?;
            return Err(EngramTransportError::local_state(
                "Retained authorization belongs to different settings; cancel or reconcile it first",
            ));
        }
        let EngramControlRequest::TurnEvaluate { routing_token, .. } = &prepared.request else {
            return Err(EngramTransportError::local_state(
                "Invalid retained evaluation request",
            ));
        };
        target.routing_token = Some(routing_token.clone());
        target.rebind_required = false;
        target.circuit_open = false;
        target.next_bind_retry_at = None;
        if cold {
            let started_at = target
                .admission_started_at
                .expect("admission budget is set");
            let timeout = target
                .remaining_dispatch_timeout(started_at)
                .ok_or_else(|| {
                    EngramTransportError::deadline("Admission recovery budget exhausted")
                })?;
            self.require_queued_engram_owner(
                &target.connection.session_id,
                owner,
                "Recovered session status",
            )?;
            let status = target
                .adapter
                .request(
                    &target.connection,
                    &EngramControlRequest::SessionStatus {
                        routing_token: routing_token.clone(),
                    },
                    timeout,
                )
                .and_then(parse_engram_result::<EngramSessionStatusResponse>)?;
            self.require_queued_engram_owner(
                &target.connection.session_id,
                owner,
                "Recovered session status",
            )?;
            if prepared.begun_grant_id.is_some()
                || target.active_grant_id.is_some()
                || status.open_grant_state.as_deref() == Some("begun")
            {
                // A begin receipt cannot prove that the provider never saw the
                // prompt. Close that grant, retain the interrupted prompt, and
                // never drain it under a fresh grant after a host restart.
                self.interrupt_queued_engram_admission(&target.connection.session_id, owner)?;
                // Commit the interruption before closing the last remote
                // evidence of possible provider delivery.
                self.confirm_engram_admission_durable(
                    &target.connection.session_id,
                    started_at,
                    owner,
                )?;
                if let Some(grant_id) = status.open_grant_id {
                    let timeout =
                        target
                            .remaining_dispatch_timeout(started_at)
                            .ok_or_else(|| {
                                EngramTransportError::deadline(
                                    "Admission recovery budget exhausted",
                                )
                            })?;
                    self.require_queued_engram_owner(
                        &target.connection.session_id,
                        owner,
                        "Recovered grant checkpoint",
                    )?;
                    let response = target
                        .adapter
                        .request(
                            &target.connection,
                            &EngramControlRequest::TurnCheckpoint {
                                routing_token: routing_token.clone(),
                                grant_id: grant_id.clone(),
                                next_intent: EngramNextIntent::Wait,
                                report: EngramTurnReport::default(),
                                idempotency_key: engram_checkpoint_idempotency_key(
                                    format!(
                                        "termal-restart-checkpoint:{}:{grant_id}",
                                        target.connection.session_id
                                    ),
                                    &EngramTurnReport::default(),
                                ),
                            },
                            timeout,
                        )
                        .and_then(parse_engram_result::<EngramTurnCheckpointResponse>)?;
                    self.require_queued_engram_owner(
                        &target.connection.session_id,
                        owner,
                        "Recovered grant checkpoint",
                    )?;
                    let checkpointed = match response {
                        EngramTurnCheckpointResponse::Checkpointed { receipt }
                            if receipt.grant_id == grant_id =>
                        {
                            true
                        }
                        _ => false,
                    };
                    if !checkpointed {
                        return Err(EngramTransportError::local_state(
                            "Interrupted grant checkpoint was not acknowledged",
                        ));
                    }
                }
                return Err(EngramTransportError::local_state(
                    "Interrupted/unknown provider delivery; retained prompt requires explicit resolution",
                ));
            }
            if status.open_grant_id.is_some()
                && status.open_grant_state.as_deref() != Some("issued")
            {
                return Err(EngramTransportError::local_state(
                    "Unknown recovered grant state; withholding delivery without rebinding",
                ));
            }
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&target.connection.session_id)
                .ok_or_else(|| EngramTransportError::local_state("Recovery session disappeared"))?;
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if !owner.matches(record) {
                return Err(EngramTransportError::local_state(
                    "Recovery no longer owns the queued prompt",
                ));
            }
            record.engram.recovered_admission = false;
        }
        Ok(true)
    }

    fn interrupt_queued_engram_admission(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
    ) -> std::result::Result<(), EngramTransportError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id) {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if !owner.matches(record) {
                return Err(EngramTransportError::local_state(
                    "Recovery no longer owns the queued prompt",
                ));
            }
            if let Some(queued) = record.queued_prompts.front_mut() {
                queued.engram_interrupted = true;
            }
            record.engram.recovered_admission = false;
            record.set_auto_dispatch_blocked(true);
            record.session.preview = "Engram: interrupted/unknown delivery. Prompt retained; cancel or reconcile before continuing.".to_owned();
            // The owner match fences the exact retained queue head and
            // generation. Only lower Active when Engram still owns the live
            // turn; a concurrent Stop (or its rollback) owns that transition.
            if record.session.status == SessionStatus::Active && !record.runtime_stop_in_progress {
                record.session.status = SessionStatus::Idle;
                record.session.live_activity = None;
                clear_active_turn_file_change_tracking(record);
            }
            sync_pending_prompts(record);
            if let Err(error) = self.commit_locked(&mut inner) {
                // `commit_locked` publishes only after persistence dispatch
                // succeeds. A failed synchronous fallback still leaves a
                // fail-closed in-memory barrier that live clients must see.
                self.publish_state_locked(&inner);
                return Err(EngramTransportError::local_state(format!(
                    "Failed to persist interrupted authorization: {error:#}"
                )));
            }
        }
        Ok(())
    }

    fn queued_engram_bind_request(
        &self,
        target: &EngramBindingTarget,
        started_at: std::time::Instant,
        owner: &EngramQueuedAdmissionOwner,
        rejected_request: Option<&EngramControlRequest>,
    ) -> std::result::Result<EngramControlRequest, EngramTransportError> {
        let queued = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&target.connection.session_id)
                .ok_or_else(|| EngramTransportError::local_state("Queued session disappeared"))?;
            if !owner.matches(&inner.sessions[index]) {
                return Err(EngramTransportError::local_state(
                    "Bind preparation no longer owns the queued prompt",
                ));
            }
            inner.sessions[index].queued_prompts.front().cloned()
        };
        if let Some(prepared) = queued
            .as_ref()
            .and_then(|queued| queued.engram_bind.as_ref())
        {
            if prepared.connection != target.connection
                || !prepared.settings.same_admission_settings(&target.settings)
            {
                return Err(EngramTransportError::local_state(
                    "Queued Engram bind belongs to different settings; reconcile or cancel the retained prompt",
                ));
            }
            if rejected_request.is_none() {
                self.confirm_engram_admission_durable(
                    &target.connection.session_id,
                    started_at,
                    owner,
                )?;
                return Ok(prepared.request.clone());
            }
            if serde_json::to_value(&prepared.request).ok()
                != rejected_request.and_then(|request| serde_json::to_value(request).ok())
            {
                return Err(EngramTransportError::local_state(
                    "Stale bind retry no longer owns the rejected request",
                ));
            }
        }
        let timeout = target
            .remaining_dispatch_timeout(started_at)
            .ok_or_else(|| {
                EngramTransportError::deadline(
                    "Engram admission budget exhausted before work focus",
                )
            })?;
        self.require_queued_engram_owner(
            &target.connection.session_id,
            owner,
            "Engram work-binding read",
        )?;
        // A rebind the admission refresh armed binds what the refresh read. A
        // stale retry, or any other bind, reads again, and a binding Engram
        // just refused as stale is not resent.
        let work_binding = match (&target.refreshed_work_binding, rejected_request) {
            (Some(refreshed), None) => refreshed.clone(),
            _ => {
                let (current, refused) =
                    self.engram_work_binding_preference(&target.connection.session_id);
                let read = target.adapter.read_work_binding(
                    &target.connection,
                    EngramBindingPreference {
                        current: current.as_ref(),
                        refused: &refused,
                    },
                    timeout,
                )?;
                self.engram_work_binding_for_bind(&target.connection.session_id, read)
            }
        };
        let key = queued.as_ref().map_or_else(
            || {
                format!(
                    "termal-bind:{}:{}",
                    target.connection.session_id,
                    Uuid::new_v4()
                )
            },
            |queued| {
                format!(
                    "termal-bind:{}:{}:{}",
                    target.connection.session_id,
                    queued.pending_prompt.id,
                    sha256_hex(
                        format!(
                            "{}:{}",
                            target.routing_token.as_deref().unwrap_or("unbound"),
                            serde_json::to_string(&work_binding).expect("work binding serializes")
                        )
                        .as_bytes()
                    )
                )
            },
        );
        let request = EngramControlRequest::SessionBind {
            external_ref: target.external_ref.clone(),
            title: target.title.clone(),
            assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
            mediated_effects: target.effects.clone(),
            capability_map_revision: ENGRAM_CAPABILITY_MAP_REVISION,
            work_binding,
            idempotency_key: key,
        };
        if let Some(queued) = queued {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&target.connection.session_id)
                .ok_or_else(|| {
                    EngramTransportError::local_state("Session was removed during work focus")
                })?;
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            if !owner.matches(record) {
                return Err(EngramTransportError::local_state(
                    "Bind preparation no longer owns the queued prompt",
                ));
            }
            let operation_generation = record.engram.dispatch_generation.saturating_add(1);
            let current = record
                .queued_prompts
                .front_mut()
                .filter(|current| {
                    current.pending_prompt.id == queued.pending_prompt.id
                        && !current.engram_interrupted
                        && rejected_request.is_none_or(|rejected| {
                            current.engram_bind.as_ref().is_some_and(|prepared| {
                                serde_json::to_value(&prepared.request).ok()
                                    == serde_json::to_value(rejected).ok()
                            })
                        })
                })
                .ok_or_else(|| {
                    EngramTransportError::local_state(
                        "Queued prompt was canceled during work focus",
                    )
                })?;
            let retained_before = current.is_engram_retained();
            current.engram_bind = Some(EngramQueuedBind {
                connection: target.connection.clone(),
                settings: target.settings.clone(),
                request: request.clone(),
                operation_generation: Some(operation_generation),
            });
            let retention_became_visible = !retained_before && current.is_engram_retained();
            sync_pending_prompts(record);
            if retention_became_visible {
                if let Err(error) = self.commit_locked(&mut inner) {
                    let record = inner
                        .session_mut_by_index(index)
                        .expect("session index should be valid");
                    if let Some(queued) = record.queued_prompts.front_mut() {
                        queued.engram_interrupted = true;
                    }
                    record.set_auto_dispatch_blocked(true);
                    record.session.preview = "Engram bind persistence is unknown. Prompt retained; reconcile before continuing.".to_owned();
                    sync_pending_prompts(record);
                    self.publish_state_locked(&inner);
                    return Err(EngramTransportError::local_state(format!(
                        "Failed to persist prepared bind: {error:#}"
                    )));
                }
            } else {
                self.persist_internal_locked(&inner).map_err(|error| {
                    EngramTransportError::local_state(format!(
                        "Failed to persist prepared bind: {error:#}"
                    ))
                })?;
            }
        }
        self.confirm_engram_admission_durable(&target.connection.session_id, started_at, owner)?;
        Ok(request)
    }

    fn queued_engram_evaluate_request(
        &self,
        target: &EngramBindingTarget,
        intent: &EngramTurnIntentSnapshot,
        owner: &EngramQueuedAdmissionOwner,
        request: EngramControlRequest,
    ) -> std::result::Result<EngramControlRequest, EngramTransportError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&intent.session_id)
            .ok_or_else(|| {
                EngramTransportError::local_state("Session was removed before evaluation")
            })?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !owner.matches(record) {
            return Err(EngramTransportError::local_state(
                "Evaluation preparation no longer owns the queued prompt",
            ));
        }
        let Some(queued) = record.queued_prompts.front_mut() else {
            return Err(EngramTransportError::local_state(
                "Queued prompt was canceled before evaluation",
            ));
        };
        if queued.engram_interrupted {
            return Err(EngramTransportError::local_state(
                "Interrupted Engram authorization requires explicit reconciliation; prompt retained",
            ));
        }
        if engram_turn_intent_fingerprint(
            &queued.pending_prompt.text,
            queued.pending_prompt.expanded_text.as_deref(),
            &queued.attachments,
            queued.pending_prompt.source.as_ref(),
            queued.source,
        ) != intent.intent_fingerprint
        {
            return Err(EngramTransportError::local_state(
                "Queued prompt changed before evaluation",
            ));
        }
        if let Some(prepared) = &queued.engram_evaluate {
            if prepared.connection != target.connection
                || !prepared.settings.same_admission_settings(&target.settings)
            {
                return Err(EngramTransportError::local_state(
                    "Queued Engram evaluation belongs to different settings; reconcile or cancel the retained prompt",
                ));
            }
            let matches_queued_intent = match &prepared.request {
                EngramControlRequest::TurnEvaluate {
                    intent_fingerprint, ..
                } if intent_fingerprint == &intent.intent_fingerprint => true,
                _ => false,
            };
            if !matches_queued_intent {
                return Err(EngramTransportError::local_state(
                    "Retained evaluation does not match the queued prompt",
                ));
            }
            let request = prepared.request.clone();
            drop(inner);
            self.confirm_engram_admission_durable(
                &intent.session_id,
                target
                    .admission_started_at
                    .unwrap_or_else(std::time::Instant::now),
                owner,
            )?;
            return Ok(request);
        }
        let retained_before = queued.is_engram_retained();
        queued.engram_evaluate = Some(EngramQueuedEvaluate {
            connection: target.connection.clone(),
            settings: target.settings.clone(),
            request: request.clone(),
            operation_generation: Some(intent.dispatch_generation),
            begun_grant_id: None,
        });
        let retention_became_visible = !retained_before && queued.is_engram_retained();
        sync_pending_prompts(record);
        if retention_became_visible {
            if let Err(error) = self.commit_locked(&mut inner) {
                let record = inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                if let Some(queued) = record.queued_prompts.front_mut() {
                    queued.engram_interrupted = true;
                }
                record.set_auto_dispatch_blocked(true);
                record.session.preview = "Engram evaluation persistence is unknown. Prompt retained; reconcile before continuing.".to_owned();
                sync_pending_prompts(record);
                self.publish_state_locked(&inner);
                return Err(EngramTransportError::local_state(format!(
                    "Failed to persist prepared evaluate: {error:#}"
                )));
            }
        } else {
            self.persist_internal_locked(&inner).map_err(|error| {
                EngramTransportError::local_state(format!(
                    "Failed to persist prepared evaluate: {error:#}"
                ))
            })?;
        }
        drop(inner);
        self.confirm_engram_admission_durable(
            &intent.session_id,
            target
                .admission_started_at
                .unwrap_or_else(std::time::Instant::now),
            owner,
        )?;
        Ok(request)
    }

    fn queued_engram_begin_key(&self, session_id: &str, generation: u64, grant_id: &str) -> String {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let key = inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions[index].queued_prompts.front())
            .and_then(|queued| queued.engram_evaluate.as_ref())
            .and_then(|prepared| match &prepared.request {
                EngramControlRequest::TurnEvaluate {
                    idempotency_key, ..
                } => Some(idempotency_key),
                _ => None,
            });
        key.map_or_else(
            || format!("termal-begin:{session_id}:{generation}:{grant_id}"),
            |key| format!("termal-begin:{}:{grant_id}", sha256_hex(key.as_bytes())),
        )
    }
}
