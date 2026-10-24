// Capacity recovery owns a bounded Resume budget, never a replayable prompt.
// Driven by the existing shared retry tick; no additional timer/thread.
const PROVIDER_CAPACITY_DELAYS_SECONDS: [u64; 8] = [120, 300, 600, 600, 600, 600, 600, 600];

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ProviderCapacityRetry {
    nonce: String,
    server_instance: String,
    runtime_id: String,
    generation: u64,
    stop_generation: u64,
    pause_epoch: u64,
    dispatch_generation: u64,
    settings: String,
    attempts: usize,
    due_at: Option<String>,
    recovering: bool,
    dispatched: bool,
    original_preview: String,
    #[serde(skip)]
    saved: bool,
    #[serde(skip)]
    fence: Option<EngramAbortAckWaiter>,
}

fn provider_capacity_delay(session_id: &str, attempt: usize) -> chrono::Duration {
    let base = PROVIDER_CAPACITY_DELAYS_SECONDS[attempt.min(7)] * 1000;
    let digest = sha256_hex(format!("capacity:{session_id}:{attempt}").as_bytes());
    let spread = u64::from_str_radix(&digest[..8], 16).unwrap_or(0) % 41;
    chrono::Duration::milliseconds((base * (80 + spread) / 100) as i64)
}

fn is_typed_provider_capacity(error: &Value) -> bool {
    error.get("codexErrorInfo").and_then(Value::as_str) == Some("serverOverloaded")
}

fn provider_capacity_settings(record: &SessionRecord) -> String {
    sha256_hex(
        json!({
            "agent": record.session.agent,
            "model": record.session.model, "workdir": record.session.workdir,
            "project": record.session.project_id, "approval": record.codex_approval_policy,
            "sandbox": record.codex_sandbox_mode, "effort": record.codex_reasoning_effort,
            "reset": record.runtime_reset_required
        })
        .to_string()
        .as_bytes(),
    )
}

fn provider_capacity_manual_hold(record: &SessionRecord) -> bool {
    provider_capacity_manual_hold_except_owned_admission(record, false)
}

fn provider_capacity_manual_hold_except_owned_admission(
    record: &SessionRecord,
    owned: bool,
) -> bool {
    record.codex_terminal_identity_unresolved
        || record.engram.operator_paused
        || record.runtime_stop_in_progress
        || record.engram.uncertain_grant_id.is_some()
        || record.engram.abort_retry.is_some()
        || record.engram.bind_retry.is_some()
        || record.engram.admission_retry.is_some()
        || record.engram.pending_dispatch.is_some()
        || (!owned && record.engram.admission_in_progress.is_some())
        || record.engram.source_observation_gate.is_some()
        || record.engram_boot_recovery_pending
        || record.engram.disabled_reason.is_some()
        || record.engram_mcp_runtime_quarantined
        || record
            .queued_prompts
            .front()
            .is_some_and(|head| head.engram_interrupted)
        || record.prompt_deliveries.values().any(|delivery| {
            delivery.disposition == PromptDeliveryDisposition::Ambiguous
                && delivery.turn_generation == Some(record.active_turn_generation)
        })
}

fn provider_capacity_owns_ack_admission(record: &SessionRecord, nonce: &str) -> bool {
    let Some(progress) = &record.engram.admission_in_progress else {
        return false;
    };
    let Some(attempt) = &record.engram.admission_attempt else {
        return false;
    };
    let Some(head) = record.queued_prompts.front() else {
        return false;
    };
    let prompt_id = &head.pending_prompt.id;
    let Some(delivery) = record.prompt_deliveries.get(prompt_id) else {
        return false;
    };
    Arc::ptr_eq(progress, &attempt.token)
        && record.engram.admission_prompt_id.as_ref() == Some(prompt_id)
        && attempt.prompt_id == *prompt_id
        && attempt.write_ahead == delivery.write_ahead
        && delivery
            .resume
            .as_ref()
            .is_some_and(|resume| resume.capacity_nonce.as_deref() == Some(nonce))
        && prompt_delivery_resume_holds(record, prompt_id)
}

fn provider_capacity_resume_owned(record: &SessionRecord, nonce: &str) -> bool {
    record
        .provider_capacity_retry
        .as_ref()
        .is_some_and(|retry| {
            retry.nonce == nonce
                && retry.recovering
                && retry.dispatched
                && retry.generation == record.active_turn_generation
                && retry.stop_generation == record.runtime_stop_generation
                && retry.pause_epoch == record.auto_dispatch_pause_epoch
                && record
                    .runtime
                    .matches_runtime_token(&RuntimeToken::Codex(retry.runtime_id.clone()))
                && retry.settings == provider_capacity_settings(record)
                && !record.runtime_stop_in_progress
                && !record.engram.operator_paused
                && record.engram.disabled_reason.is_none()
        })
}

fn clear_provider_capacity_retry(record: &mut SessionRecord) -> bool {
    record.session.provider_capacity_waiting = false;
    let Some(retry) = record.provider_capacity_retry.take() else {
        return false;
    };
    if record
        .session
        .preview
        .starts_with("Waiting: provider at capacity,")
        || record
            .session
            .preview
            .starts_with("Paused: provider at capacity, retries exhausted")
    {
        record.session.status = SessionStatus::Error;
        record.session.preview = retry.original_preview;
    }
    true
}

impl AppState {
    fn provider_capacity_failure_already_scheduled(
        &self,
        session_id: &str,
        runtime: &RuntimeToken,
        generation: Option<u64>,
    ) -> bool {
        let Some(generation) = generation else {
            return false;
        };
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner.find_session_index(session_id).is_some_and(|index| {
            let record = &inner.sessions[index];
            record
                .provider_capacity_retry
                .as_ref()
                .is_some_and(|retry| {
                    retry.server_instance == self.server_instance_id
                        && retry.generation == generation
                        && record.active_turn_generation == generation
                        && record.runtime.matches_runtime_token(runtime)
                        && runtime == &RuntimeToken::Codex(retry.runtime_id.clone())
                })
        })
    }

    fn provider_capacity_event_is_current(
        &self,
        session_id: &str,
        generation: Option<u64>,
    ) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        generation.is_none_or(|generation| {
            inner
                .find_session_index(session_id)
                .is_some_and(|index| inner.sessions[index].active_turn_generation == generation)
        })
    }
    fn cancel_provider_capacity_retry(&self, session_id: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id)
            && clear_provider_capacity_retry(&mut inner.sessions[index])
        {
            inner.stamp_session_at_index(index);
            if let Err(error) = self.commit_locked(&mut inner) {
                eprintln!("capacity> failed persisting cancellation: {error:#}");
            }
        }
    }

    fn cancel_provider_capacity_retry_if_runtime_and_generation_match(
        &self,
        session_id: &str,
        runtime: &RuntimeToken,
        generation: u64,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = &mut inner.sessions[index];
        if !record.runtime.matches_runtime_token(runtime)
            || record.active_turn_generation != generation
        {
            return;
        }
        if clear_provider_capacity_retry(record) {
            inner.stamp_session_at_index(index);
            if let Err(error) = self.commit_locked(&mut inner) {
                eprintln!("capacity> failed persisting cancellation: {error:#}");
            }
        }
    }

    // Arm before generic terminal handling so its queue drain cannot start a
    // successor. Finalize only if that exact owner survived terminal handling.
    fn prepare_provider_capacity_failure(
        &self,
        session_id: &str,
        runtime: &RuntimeToken,
        generation: Option<u64>,
        confirmed_terminal: bool,
        error: &Value,
        compacted: bool,
    ) -> Option<String> {
        if !confirmed_terminal || compacted || !is_typed_provider_capacity(error) {
            return None;
        }
        let RuntimeToken::Codex(runtime_id) = runtime else {
            return None;
        };
        let generation = generation?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner
            .delegations
            .iter()
            .any(|delegation| delegation.child_session_id == session_id)
        {
            return None;
        }
        let index = inner.find_visible_session_index(session_id)?;
        let record = &mut inner.sessions[index];
        if !record.is_local_session()
            || record.session.agent != Agent::Codex
            || record.runtime_reset_required
            || !record.runtime.matches_runtime_token(runtime)
            || record.active_turn_generation != generation
            || provider_capacity_manual_hold(record)
        {
            return None;
        }
        let attempts = match record.provider_capacity_retry.as_ref() {
            Some(retry)
                if retry.recovering
                    && retry.dispatched
                    && retry.generation.wrapping_add(1).max(1) == generation
                    && retry.stop_generation == record.runtime_stop_generation
                    && retry.settings == provider_capacity_settings(record) =>
            {
                retry.attempts
            }
            _ if record.orchestrator_auto_dispatch_blocked => return None,
            _ => 0,
        };
        record.set_auto_dispatch_blocked(true);
        let nonce = Uuid::new_v4().to_string();
        record.provider_capacity_retry = Some(ProviderCapacityRetry {
            nonce: nonce.clone(),
            server_instance: self.server_instance_id.clone(),
            runtime_id: runtime_id.clone(),
            generation,
            stop_generation: record.runtime_stop_generation,
            pause_epoch: record.auto_dispatch_pause_epoch,
            dispatch_generation: record.engram.dispatch_generation,
            settings: provider_capacity_settings(record),
            attempts,
            due_at: None,
            recovering: false,
            dispatched: false,
            original_preview: summarize_error(error),
            saved: false,
            fence: None,
        });
        // Terminal finalization uses a later lock acquisition. Other producers
        // may publish the paused queue before then, so invalidate it now.
        inner.stamp_session_at_index(index);
        Some(nonce)
    }

    fn finish_provider_capacity_failure(&self, session_id: &str, nonce: &str) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(());
        };
        let record = &mut inner.sessions[index];
        let Some(mut retry) = record
            .provider_capacity_retry
            .clone()
            .filter(|r| r.nonce == nonce)
        else {
            return Ok(());
        };
        if record.active_turn_generation != retry.generation
            || !record.orchestrator_auto_dispatch_blocked
            || record.runtime_stop_generation != retry.stop_generation
            || !record
                .runtime
                .matches_runtime_token(&RuntimeToken::Codex(retry.runtime_id.clone()))
            || provider_capacity_manual_hold(record)
            || record.session.status != SessionStatus::Error
        {
            clear_provider_capacity_retry(record);
        } else {
            retry.pause_epoch = record.auto_dispatch_pause_epoch;
            retry.dispatch_generation = record.engram.dispatch_generation;
            retry.original_preview = record.session.preview.clone();
            if retry.attempts >= 8 {
                record.session.preview = "Paused: provider at capacity, retries exhausted (8 attempts, last error serverOverloaded); Resume to retry".to_owned();
            } else {
                let due = chrono::Utc::now() + provider_capacity_delay(session_id, retry.attempts);
                retry.due_at = Some(due.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
                record.session.status = SessionStatus::Idle;
                record.session.provider_capacity_waiting = true;
                record.session.preview = format!(
                    "Waiting: provider at capacity, retry {}/8 at {}",
                    retry.attempts + 1,
                    due.format("%H:%M")
                );
            }
            record.provider_capacity_retry = Some(retry);
        }
        inner.stamp_session_at_index(index);
        self.commit_locked(&mut inner)?;
        self.request_provider_capacity_ack_locked(&mut inner, index);
        Ok(())
    }

    fn request_provider_capacity_ack_locked(&self, inner: &mut StateInner, index: usize) {
        let Some(retry) = inner.sessions[index].provider_capacity_retry.as_ref() else {
            return;
        };
        let clock = inner.engram_budget_clock_snapshot();
        let (fence, waiter) = PersistFence::new_with_clock(
            PersistFenceTarget::ProviderCapacityRecovery {
                session_id: inner.sessions[index].session.id.clone(),
                content: serde_json::to_value(Some(retry)).expect("capacity record serializable"),
            },
            clock.now() + ENGRAM_ABORT_SETTLEMENT_FENCE,
            clock,
        );
        if self
            .persist_tx
            .send(PersistRequest::Fence(Box::new(
                fence.retry_acknowledgement(),
            )))
            .is_ok()
        {
            inner.sessions[index]
                .provider_capacity_retry
                .as_mut()
                .unwrap()
                .fence = Some(EngramAbortAckWaiter(Arc::new(waiter)));
        } else if self.persist_internal_locked(inner).is_ok() {
            inner.sessions[index]
                .provider_capacity_retry
                .as_mut()
                .unwrap()
                .saved = true;
        }
    }

    fn provider_capacity_retry_tick(&self, now: chrono::DateTime<chrono::Utc>) {
        let ids = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter(|r| r.provider_capacity_retry.is_some())
                .map(|r| r.session.id.clone())
                .collect::<Vec<_>>()
        };
        for session_id in ids {
            let Some(_slot) = self.engram_retry_slots.try_acquire_opportunistic() else {
                break;
            };
            let attempt = {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                let Some(index) = inner.find_session_index(&session_id) else {
                    continue;
                };
                let record = &mut inner.sessions[index];
                let Some(retry) = record.provider_capacity_retry.as_ref() else {
                    continue;
                };
                // Dispatched turns are settled by their terminal event, never
                // by another tick. Any new failure must be typed again.
                if retry.dispatched
                    && retry.stop_generation == record.runtime_stop_generation
                    && retry.settings == provider_capacity_settings(record)
                    && record
                        .runtime
                        .matches_runtime_token(&RuntimeToken::Codex(retry.runtime_id.clone()))
                    && !record.engram.operator_paused
                    && record.active_turn_generation == retry.generation.wrapping_add(1).max(1)
                {
                    continue;
                }
                let owns_ack_admission = provider_capacity_owns_ack_admission(record, &retry.nonce);
                let owns = retry.server_instance == self.server_instance_id
                    && retry.generation == record.active_turn_generation
                    && retry.stop_generation == record.runtime_stop_generation
                    && retry.pause_epoch == record.auto_dispatch_pause_epoch
                    && retry.dispatch_generation == record.engram.dispatch_generation
                    && retry.settings == provider_capacity_settings(record)
                    && record.orchestrator_auto_dispatch_blocked
                    && record
                        .runtime
                        .matches_runtime_token(&RuntimeToken::Codex(retry.runtime_id.clone()))
                    && !provider_capacity_manual_hold_except_owned_admission(
                        record,
                        owns_ack_admission,
                    );
                if !owns {
                    clear_provider_capacity_retry(record);
                    inner.stamp_session_at_index(index);
                    let _ = self.commit_locked(&mut inner);
                    continue;
                }
                if !retry.saved {
                    let saved = retry
                        .fence
                        .as_ref()
                        .and_then(|waiter| waiter.0.wait_until(waiter.0.now()));
                    if matches!(saved, Some(Ok(()))) {
                        record.provider_capacity_retry.as_mut().unwrap().saved = true;
                    } else {
                        if retry.fence.is_none() || saved.is_some() {
                            self.request_provider_capacity_ack_locked(&mut inner, index);
                        }
                        continue;
                    }
                }
                let retry = record.provider_capacity_retry.as_mut().unwrap();
                if retry.dispatched {
                    continue;
                }
                if !retry.recovering {
                    if retry
                        .due_at
                        .as_ref()
                        .is_none_or(|due| !engram_retry_due_passed(due, now))
                    {
                        continue;
                    }
                    retry.attempts += 1;
                    retry.recovering = true;
                    retry.due_at = None;
                    retry.saved = false;
                    retry.fence = None;
                    // Save the charged attempt before Resume may expose it.
                    inner.stamp_session_at_index(index);
                    if self.commit_locked(&mut inner).is_ok() {
                        self.request_provider_capacity_ack_locked(&mut inner, index);
                    }
                    continue;
                }
                retry.dispatched = true;
                record.session.provider_capacity_waiting = false;
                let nonce = retry.nonce.clone();
                record.session.status = SessionStatus::Error;
                record.session.preview = retry.original_preview.clone();
                let mut owner = EngramQueuedAdmissionOwner::capture(record);
                if let Some(owner) = &mut owner {
                    owner.capacity_nonce = Some(nonce.clone());
                }
                inner.stamp_session_at_index(index);
                if self.commit_locked(&mut inner).is_err() {
                    inner.sessions[index]
                        .provider_capacity_retry
                        .as_mut()
                        .unwrap()
                        .dispatched = false;
                    continue;
                }
                (nonce, owner)
            };
            let (nonce, owner) = attempt;
            let resumed = self.resume_local_session_queue_inner(&session_id, owner, Some(&nonce));
            match resumed {
                Ok(Some(
                    TurnDispatchDeliveryOutcome::Held { .. }
                    | TurnDispatchDeliveryOutcome::Superseded,
                )) => {
                    self.cancel_provider_capacity_retry(&session_id);
                }
                Ok(Some(delivery)) => {
                    if let Err(error) = delivery.into_background_result("provider capacity Resume")
                    {
                        eprintln!("capacity> Resume delivery failed: {}", error.message);
                        self.cancel_provider_capacity_retry(&session_id);
                    }
                }
                // An empty queue or any held/refused admission ends this
                // schedule. It cannot loosen the admission's own hold.
                Ok(None) | Err(_) => self.cancel_provider_capacity_retry(&session_id),
            }
        }
    }
}
