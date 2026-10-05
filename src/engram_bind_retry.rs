// Retained-bind retry is distinct from a settled-abort fresh admission. It
// keeps the exact prepared wire request and uses the existing retry tick and
// admission durability fence. Only a live pre-evaluate boundary creates proof;
// neither a card, a saved journal nor a closed remote grant can create it.

#[cfg(test)]
type BindDispositionGateKey = (usize, String, &'static str);
#[cfg(test)]
type BindDispositionGateChannels = (mpsc::SyncSender<()>, mpsc::Receiver<()>);
#[cfg(test)]
static BIND_DISPOSITION_GATES: LazyLock<Mutex<HashMap<BindDispositionGateKey, BindDispositionGateChannels>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
struct BindDispositionGate {
    key: BindDispositionGateKey,
    entered: mpsc::Receiver<()>,
    release: mpsc::SyncSender<()>,
}

#[cfg(test)]
impl BindDispositionGate {
    fn new(state: &AppState, session: &str, phase: &'static str) -> Self {
        let key = (Arc::as_ptr(&state.inner) as usize, session.to_owned(), phase);
        let (entered_tx, entered) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        assert!(BIND_DISPOSITION_GATES.lock().unwrap().insert(key.clone(), (entered_tx, release_rx)).is_none());
        Self { key, entered, release }
    }

    fn wait(&self) {
        self.entered.recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
            .expect("actual bind disposition caller should reach its gate");
    }

    fn release(self) {
        let _ = self.release.send(());
    }
}

#[cfg(test)]
impl Drop for BindDispositionGate {
    fn drop(&mut self) {
        BIND_DISPOSITION_GATES.lock().unwrap().remove(&self.key);
        // Dropping the sender also releases a worker already at the gate.
    }
}

#[cfg(test)]
fn wait_at_bind_disposition_gate(state: &AppState, session: &str, phase: &'static str) {
    let key = (Arc::as_ptr(&state.inner) as usize, session.to_owned(), phase);
    let gate = BIND_DISPOSITION_GATES.lock().unwrap().remove(&key);
    if let Some((entered, release)) = gate {
        let _ = entered.send(());
        match release.recv_timeout(TEST_PHASE_DEADLOCK_GUARD) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("bind disposition gate was not released"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum EngramBindRetryPhase {
    BeforePreparation,
    Prepared { request_digest: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct EngramBindRetryProof {
    prompt_id: String,
    fingerprint: String,
    authority: String,
    dispatch_generation: u64,
    #[serde(skip)]
    generation_before_promotion: u64,
    /// Promotion is the only allowed transfer from this admission's owner.
    promoted_turn_generation: u64,
    #[serde(skip)]
    promotion_index: Option<usize>,
    #[serde(skip)]
    runtime_before_promotion: Option<RuntimeToken>,
    /// Escalation belongs to the captured attempt even if the old journal is
    /// retired while this admission is in flight.
    #[serde(default)]
    previous_attempts: u32,
    #[serde(skip)]
    retry_eligible: bool,
    phase: EngramBindRetryPhase,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramBindRetry {
    proof: EngramBindRetryProof,
    attempts: u32,
    due_at: String,
    acknowledged: bool,
    /// When the prompt was first held (RFC 3339); later attempts keep it. A
    /// restart carries it into the parked-admission retry that replaces this
    /// record (`engram_admission_retry.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    held_since: Option<String>,
}

fn engram_bind_retry_phase(head: &QueuedPromptRecord) -> EngramBindRetryPhase {
    head.engram_bind
        .as_ref()
        .map_or(EngramBindRetryPhase::BeforePreparation, |bind| {
            EngramBindRetryPhase::Prepared {
                request_digest: sha256_hex(
                    serde_json::to_string(bind)
                        .expect("bind is serializable")
                        .as_bytes(),
                ),
            }
        })
}

fn engram_bind_retry_prompt_matches(
    head: &QueuedPromptRecord,
    proof: &EngramBindRetryProof,
) -> bool {
    head.pending_prompt.id == proof.prompt_id
        && engram_turn_intent_fingerprint(
            &head.pending_prompt.text,
            head.pending_prompt.expanded_text.as_deref(),
            &head.attachments,
            head.pending_prompt.source.as_ref(),
            head.source,
        ) == proof.fingerprint
}

fn engram_bind_retry_head_matches(record: &SessionRecord, proof: &EngramBindRetryProof) -> bool {
    record.engram.dispatch_generation == proof.dispatch_generation
        && record.active_turn_generation == proof.promoted_turn_generation
        && record.queued_prompts.front().is_some_and(|head| {
            engram_bind_retry_prompt_matches(head, proof)
                && head.engram_evaluate.is_none()
                && engram_bind_retry_phase(head) == proof.phase
        })
}

fn engram_bind_retry_releases(record: &SessionRecord, authority: Option<&str>) -> bool {
    record.engram.bind_retry.as_ref().is_some_and(|retry| {
        retry.acknowledged
            && !record.dedicated_cleanup_holds_admission()
            && record.engram.abort_retry_acknowledged
            && authority == Some(retry.proof.authority.as_str())
            && engram_bind_retry_head_matches(record, &retry.proof)
            && record
                .engram
                .bind_retry_runtime
                .as_ref()
                .is_some_and(|runtime| record.runtime.matches_runtime_token(runtime))
            && !record.runtime_stop_in_progress
    })
}

fn clear_engram_bind_retry(record: &mut SessionRecord) {
    if record.engram.bind_retry.take().is_some() {
        record.engram.bind_retry_runtime = None;
        record.engram.abort_retry_fence = None;
        record.engram.abort_retry_acknowledged = false;
        record.engram.abort_retry_saved = false;
    }
}

impl AppState {
    fn postpone_engram_bind_retry(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let authority =
            Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .ok()
                .flatten()
                .map(|target| engram_abort_authority(&target));
        let record = &mut inner.sessions[index];
        if !owner.matches(record)
            || !engram_bind_retry_releases(record, authority.as_deref())
            || record.engram.admission_in_progress.is_some()
            || record.engram.pending_dispatch.is_some()
        {
            return;
        }
        let Some(retry) = record
            .engram
            .bind_retry
            .clone()
            .filter(|retry| retry.proof.prompt_id == owner.prompt_id)
        else {
            return;
        };
        if !engram_bind_retry_head_matches(record, &retry.proof) {
            return;
        }
        let retry = record.engram.bind_retry.as_mut().expect("matching retry");
        retry.attempts = retry.attempts.saturating_add(1);
        retry.due_at = (now + engram_abort_retry_delay(session_id, retry.attempts))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        retry.acknowledged = false;
        record.engram.abort_retry_acknowledged = false;
        record.engram.abort_retry_saved = false;
        record.engram.abort_retry_fence = None;
        inner.stamp_session_at_index(index);
        if let Err(error) = self.commit_locked(&mut inner) {
            eprintln!("engram> failed persisting postponed bind retry: {error:#}");
        }
        self.request_engram_abort_acknowledgement_locked(&mut inner, index);
    }

    /// Capture the phase before entering the bind boundary, not by examining
    /// its eventual error card. A recovered or uncertain evaluate is excluded.
    fn capture_engram_bind_retry_proof(
        &self,
        intent: &EngramTurnIntentSnapshot,
    ) -> Option<EngramBindRetryProof> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(&intent.session_id)?];
        let head = record.queued_prompts.front()?;
        let target =
            Self::engram_binding_target_for_session_shape_locked(&inner, &intent.session_id, true)
                .ok()??;
        let authority = engram_abort_authority(&target);
        let live_retry = engram_bind_retry_releases(record, Some(&authority));
        if record.engram.recovered_admission
            || record.runtime_stop_in_progress
            || record.engram.active_grant_id.is_some()
            || record.engram.uncertain_grant_id.is_some()
            || head.engram_evaluate.is_some()
            || (!live_retry && (head.is_engram_retained() || head.promoted_message_index.is_some()))
        {
            return None;
        }
        Some(EngramBindRetryProof {
            prompt_id: head.pending_prompt.id.clone(),
            fingerprint: intent.intent_fingerprint.clone(),
            authority,
            dispatch_generation: intent.dispatch_generation,
            generation_before_promotion: record.engram.dispatch_generation,
            promoted_turn_generation: record.active_turn_generation.wrapping_add(1).max(1),
            promotion_index: None,
            runtime_before_promotion: record.runtime.runtime_token(),
            previous_attempts: record
                .engram
                .bind_retry
                .as_ref()
                .filter(|retry| retry.proof.prompt_id == head.pending_prompt.id)
                .map_or(0, |retry| retry.attempts),
            retry_eligible: true,
            phase: engram_bind_retry_phase(head),
        })
    }

    /// Refresh the prepared phase after the failed bind, but only while the
    /// original admission still owns the same head and authority. No evaluate
    /// request has been prepared or sent in this typed boundary.
    fn finish_engram_bind_retry_proof(
        &self,
        intent: &EngramTurnIntentSnapshot,
        owner: Option<&EngramQueuedAdmissionOwner>,
        mut proof: EngramBindRetryProof,
    ) -> Option<EngramBindRetryProof> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(&intent.session_id)?];
        let head = record.queued_prompts.front()?;
        let target =
            Self::engram_binding_target_for_session_shape_locked(&inner, &intent.session_id, true)
                .ok()??;
        if !owner.is_some_and(|owner| owner.matches(record))
            || record.runtime.runtime_token() != proof.runtime_before_promotion
            || record.runtime_stop_in_progress
            || engram_abort_authority(&target) != proof.authority
            || head.pending_prompt.id != proof.prompt_id
            || head.engram_evaluate.is_some()
            || record.engram.active_grant_id.is_some()
            || record.engram.uncertain_grant_id.is_some()
            || engram_turn_intent_fingerprint(
                &head.pending_prompt.text,
                head.pending_prompt.expanded_text.as_deref(),
                &head.attachments,
                head.pending_prompt.source.as_ref(),
                head.source,
            ) != proof.fingerprint
        {
            return None;
        }
        proof.phase = engram_bind_retry_phase(head);
        Some(proof)
    }

    /// Called instead of the generic unknown-authorization park, before the
    /// API could hand this dispatch to a provider. Keep the bind journal intact.
    fn park_engram_bind_retry(
        &self,
        session_id: &str,
        proof: EngramBindRetryProof,
        runtime: &RuntimeToken,
        turn_generation: u64,
    ) -> EngramAuthorizationParkOutcome {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return EngramAuthorizationParkOutcome::Superseded;
        };
        let authority =
            Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .ok()
                .flatten()
                .map(|target| engram_abort_authority(&target));
        let record = &inner.sessions[index];
        // Eligibility loss is not supersession. The exact promoted dispatch
        // still owns a conservative hold, but cannot recreate retry authority.
        let owns_turn = record.engram.dispatch_generation == proof.dispatch_generation
            && record.active_turn_generation == proof.promoted_turn_generation;
        if !owns_turn
            || record.active_turn_generation != turn_generation
            || !record.runtime.matches_runtime_token(runtime)
            || record.runtime_stop_in_progress
            || record.session.status != SessionStatus::Active
        {
            return EngramAuthorizationParkOutcome::Superseded;
        }
        let same_head = record.queued_prompts.front()
            .is_some_and(|head| engram_bind_retry_prompt_matches(head, &proof));
        if !same_head && proof.phase == EngramBindRetryPhase::BeforePreparation {
            drop(inner);
            return self.withdraw_unprepared_bind_dispatch(session_id, &proof, runtime, turn_generation);
        }
        let eligible = proof.retry_eligible
            && same_head
            && record.engram.disabled_reason.is_none()
            && engram_bind_retry_head_matches(record, &proof)
            && authority.as_deref() == Some(&proof.authority)
            && !record.engram.project_reset_in_progress
            && !engram_project_for_session_locked(&inner, session_id)
                .is_some_and(|project| inner.engram_project_resets.contains(&project.id))
            && record.engram.active_grant_id.is_none()
            && record.engram.uncertain_grant_id.is_none();
        let record = inner
            .session_mut_by_index(index)
            .expect("validated session");
        if !eligible {
            clear_engram_bind_retry(record);
            record.set_auto_dispatch_blocked(true);
            record.session.status = SessionStatus::Idle;
            if let Some(head) = record.queued_prompts.front_mut() {
                head.engram_waiting = true;
                head.engram_interrupted = true;
            }
            record.session.live_activity = None;
            clear_active_turn_file_change_tracking(record);
            record.session.preview = "Engram: bind retry authority changed. Prompt retained; reconcile or cancel before continuing.".to_owned();
            sync_pending_prompts(record);
            return match self.commit_locked(&mut inner) {
                Ok(_) => EngramAuthorizationParkOutcome::Parked,
                Err(error) => {
                    eprintln!("engram> failed persisting conservative bind hold: {error:#}");
                    self.publish_state_locked(&inner);
                    EngramAuthorizationParkOutcome::PersistenceUnknown
                }
            };
        }
        let attempts = proof.previous_attempts.saturating_add(1);
        let now = chrono::Utc::now();
        let held_since = record
            .engram
            .bind_retry
            .as_ref()
            .filter(|retry| retry.proof.prompt_id == proof.prompt_id)
            .and_then(|retry| retry.held_since.clone())
            .unwrap_or_else(|| now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        record.engram.abort_retry = None;
        clear_engram_admission_retry(record);
        record.engram.bind_retry = Some(EngramBindRetry {
            proof,
            attempts,
            due_at: (now + engram_abort_retry_delay(session_id, attempts))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            acknowledged: false,
            held_since: Some(held_since),
        });
        record.engram.bind_retry_runtime = Some(runtime.clone());
        record.engram.abort_retry_acknowledged = false;
        record.engram.abort_retry_saved = false;
        record.engram.abort_retry_fence = None;
        record.set_auto_dispatch_blocked(true);
        record.session.status = SessionStatus::Idle;
        record
            .queued_prompts
            .front_mut()
            .expect("proof names head")
            .engram_waiting = true;
        record.session.live_activity = None;
        clear_active_turn_file_change_tracking(record);
        record.session.preview =
            "Engram: bind deferred before delivery; waiting for durable automatic retry."
                .to_owned();
        sync_pending_prompts(record);
        let persisted = self.commit_locked(&mut inner);
        self.request_engram_abort_acknowledgement_locked(&mut inner, index);
        if let Err(error) = persisted {
            eprintln!("engram> failed persisting retained bind retry: {error:#}");
            self.publish_state_locked(&inner);
            EngramAuthorizationParkOutcome::PersistenceUnknown
        } else {
            EngramAuthorizationParkOutcome::Parked
        }
    }

    /// Head identity is not dispatch ownership. A proven unprepared attempt
    /// can settle without reporting provider success or replaying its old wake.
    fn withdraw_unprepared_bind_dispatch(
        &self,
        session_id: &str,
        proof: &EngramBindRetryProof,
        runtime: &RuntimeToken,
        turn_generation: u64,
    ) -> EngramAuthorizationParkOutcome {
        let owner_generation = match self.claim_turn_terminalization_if_runtime_matches(
            session_id, runtime, turn_generation,
        ) {
            Ok(Some(owner)) => owner,
            Ok(None) => return EngramAuthorizationParkOutcome::Superseded,
            Err(_) => return EngramAuthorizationParkOutcome::PersistenceUnknown,
        };
        let clock = self.engram_budget_clock();
        #[cfg(test)]
        wait_at_bind_disposition_gate(
            self, session_id, "withdrawal",
        );
        let (settlement, may_continue) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return EngramAuthorizationParkOutcome::Superseded;
            };
            let authority = Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .ok().flatten().map(|target| engram_abort_authority(&target));
            let reset = engram_project_for_session_locked(&inner, session_id)
                .is_some_and(|project| inner.engram_project_resets.contains(&project.id));
            let record = &inner.sessions[index];
            if record.engram.dispatch_generation != proof.dispatch_generation
                || record.active_turn_generation != turn_generation
                || record.session.status != SessionStatus::Active
                || !record.runtime.matches_runtime_token(runtime)
                || !record.runtime_stop_is_owned_by(RuntimeStopOwnerKind::LostRuntimeTerminalization, runtime, owner_generation)
            {
                drop(inner);
                self.release_turn_terminalization_if_owned(session_id, runtime, owner_generation);
                return EngramAuthorizationParkOutcome::Superseded;
            }
            let may_continue = authority.as_deref() == Some(&proof.authority)
                && !reset && !record.engram.project_reset_in_progress
                && record.engram.disabled_reason.is_none()
                && record.engram.active_grant_id.is_none()
                && record.engram.uncertain_grant_id.is_none()
                && record.queued_prompts.front().is_none_or(|head| !head.is_engram_retained());
            let retired_position = if may_continue && proof.retry_eligible {
                record.queued_prompts.front().filter(|head| {
                    head.pending_prompt.id == proof.prompt_id
                        && proof.promotion_index.is_some()
                        && head.promoted_message_index == proof.promotion_index
                        && !head.is_engram_retained()
                }).map(|_| {
                    cached_message_index_on_record(record, &proof.prompt_id).filter(|&position| {
                        Some(global_message_index(record, position)) == proof.promotion_index
                            && matches!(&record.session.messages[position], Message::Text { author: Author::You, .. })
                    })
                })
            } else { None };
            if retired_position == Some(None) {
                drop(inner);
                self.release_turn_terminalization_if_owned(session_id, runtime, owner_generation);
                eprintln!("engram> withdrawing prompt has no reusable owned transcript position");
                return EngramAuthorizationParkOutcome::PersistenceUnknown;
            }
            let record = inner.session_mut_by_index(index).expect("owned session");
            clear_engram_bind_retry(record);
            // The changed operation identity makes read-back distinguish this
            // settled row from an earlier saved Active card with the same head.
            record.engram.dispatch_generation = record.engram.dispatch_generation.saturating_add(1);
            record.session.status = SessionStatus::Idle;
            record.session.live_activity = None;
            record.active_turn_mailbox_notification = None;
            clear_active_turn_file_change_tracking(record);
            let mut changed_positions = Vec::new();
            if let Some(Some(position)) = retired_position {
                let head = record.queued_prompts.front_mut().expect("validated surviving prompt");
                head.promoted_message_index = None;
                head.promotion_disposition_known = true;
                let current = head.pending_prompt.clone();
                record.mark_body_changed(&proof.prompt_id);
                if let Message::Text { text, expanded_text, attachments, source, .. } = &mut record.session.messages[position] {
                    *text = current.text;
                    *expanded_text = current.expanded_text;
                    *attachments = current.attachments;
                    *source = current.source;
                }
                sync_retained_transcript_metadata(record);
                if record.session.messages_loaded {
                    let history = prompt_history_from_messages(&record.session.messages);
                    set_prompt_history_on_record(record, history);
                }
                changed_positions.push(position);
            }
            if !may_continue {
                record.set_auto_dispatch_blocked(true);
                if let Some(head) = record.queued_prompts.front_mut() {
                    head.engram_waiting = true;
                    head.engram_interrupted = true;
                }
            }
            record.session.preview = if may_continue {
                "Engram: undelivered turn withdrawn.".to_owned()
            } else {
                "Engram: withdrawn turn retained behind changed authority; reconcile or cancel before continuing.".to_owned()
            };
            sync_pending_prompts(record);
            let updates = message_updated_delta_parts_for_indices(record, changed_positions);
            let target = PersistFenceTarget::EngramAdmission {
                session_id: session_id.to_owned(), content: engram_admission_live_content(record),
            };
            let settlement = self.commit_locked(&mut inner).map(|revision| {
                self.publish_message_updated_delta_parts(&inner, revision, updates);
                let (fence, waiter) = PersistFence::new_with_clock(
                    target, clock.now() + ENGRAM_ABORT_SETTLEMENT_FENCE, clock.clone(),
                );
                if self.persist_tx.send(PersistRequest::Fence(Box::new(fence))).is_ok() {
                    Some(waiter)
                } else {
                    None
                }
            }).and_then(|waiter| {
                if waiter.is_none() { self.persist_internal_locked(&inner)?; }
                Ok(waiter)
            });
            (settlement, may_continue)
        };
        let saved = settlement.and_then(|waiter| {
            if let Some(waiter) = waiter { waiter.wait().map_err(|e| anyhow!("withdrawal acknowledgement: {e:?}"))?; }
            Ok(())
        });
        if let Err(error) = saved {
            // No fresh admission after an unknown durability result.
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(index) = inner.find_session_index(session_id) {
                let record = &inner.sessions[index];
                if record.runtime_stop_is_owned_by(RuntimeStopOwnerKind::LostRuntimeTerminalization, runtime, owner_generation) {
                    inner.session_mut_by_index(index).unwrap().set_auto_dispatch_blocked(true);
                }
            }
            eprintln!("engram> failed settling undelivered bind turn: {error:#}");
            drop(inner);
            self.release_turn_terminalization_if_owned(session_id, runtime, owner_generation);
            return EngramAuthorizationParkOutcome::PersistenceUnknown;
        }
        if !self.release_turn_terminalization_if_owned(session_id, runtime, owner_generation) {
            return EngramAuthorizationParkOutcome::Superseded;
        }
        if may_continue { EngramAuthorizationParkOutcome::Withdrawn } else { EngramAuthorizationParkOutcome::Parked }
    }
}

fn engram_bind_retry_step(
    record: &mut SessionRecord,
    authority: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
    budget_now: std::time::Instant,
) -> EngramAbortRetryStep {
    let Some(retry) = record.engram.bind_retry.clone() else {
        return EngramAbortRetryStep::Wait;
    };
    if record.dedicated_cleanup_holds_admission() {
        return EngramAbortRetryStep::Wait;
    }
    if engram_retry_attempt_in_flight(record) {
        return EngramAbortRetryStep::Wait;
    }
    // A card may finish after promotion but before its park. Only the same
    // prompt's immediately following owned turn can carry this lineage gap.
    if matches!(
        record.session.status,
        SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
    ) && record.active_turn_generation
        == retry.proof.promoted_turn_generation.wrapping_add(1).max(1)
        && record.engram.dispatch_generation == retry.proof.dispatch_generation
        && authority == Some(retry.proof.authority.as_str())
        && record
            .engram
            .bind_retry_runtime
            .as_ref()
            .is_some_and(|runtime| record.runtime.matches_runtime_token(runtime))
        && record.queued_prompts.front().is_some_and(|head| {
            head.pending_prompt.id == retry.proof.prompt_id
                && engram_bind_retry_phase(head) == retry.proof.phase
                && head.engram_evaluate.is_none()
                && engram_turn_intent_fingerprint(
                    &head.pending_prompt.text,
                    head.pending_prompt.expanded_text.as_deref(),
                    &head.attachments,
                    head.pending_prompt.source.as_ref(),
                    head.source,
                ) == retry.proof.fingerprint
        })
    {
        return EngramAbortRetryStep::Wait;
    }
    if !engram_bind_retry_head_matches(record, &retry.proof)
        || authority != Some(retry.proof.authority.as_str())
        || !record
            .engram
            .bind_retry_runtime
            .as_ref()
            .is_some_and(|runtime| record.runtime.matches_runtime_token(runtime))
        || record.runtime_stop_in_progress
    {
        clear_engram_bind_retry(record);
        if matches!(record.session.status, SessionStatus::Idle | SessionStatus::Error)
            && record.queued_prompts.front().is_some_and(|head| head.pending_prompt.id == retry.proof.prompt_id)
        {
            record.session.preview = "Engram: automatic bind retry no longer owns this prompt. Prompt retained; reconcile or cancel before continuing.".to_owned();
            sync_pending_prompts(record);
        }
        return EngramAbortRetryStep::Dropped;
    }
    if let Some(step) = engram_retry_acknowledgement_step(record, |record| {
        record
            .engram
            .bind_retry
            .as_mut()
            .expect("retry exists")
            .acknowledged = true;
    }) {
        return step;
    }
    // Retained-bind restoration intentionally clears target backoff for
    // explicit reconciliation. The automatic scheduler must honour the live
    // bind/circuit backoff before it enters that canonical replay path.
    let bind_backed_off = record
        .engram
        .next_bind_retry_at
        .is_some_and(|at| at > budget_now);
    if !engram_retry_due_passed(&retry.due_at, now)
        || bind_backed_off
        || !matches!(
            record.session.status,
            SessionStatus::Idle | SessionStatus::Error
        )
        || record.engram.project_reset_in_progress
    {
        return EngramAbortRetryStep::Wait;
    }
    EngramAbortRetryStep::Due
}
