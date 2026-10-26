// Ordinary root availability before Begin. The saved head records a possible
// provider handoff, while the permission to perform it exists only in this
// process and for one exact queue/turn owner. No grant or check credit is made.

#[derive(Clone, Debug)]
struct EngramOrdinaryHandoff {
    owner: EngramQueuedAdmissionOwner,
    // Stop may preserve this unsent preparation, but only an acknowledged
    // marker permits the provider enqueue.
    durable: bool,
}

struct EngramOrdinaryPreparation<'a> {
    state: &'a AppState,
    session_id: &'a str,
    owner: &'a EngramQueuedAdmissionOwner,
}

impl Drop for EngramOrdinaryPreparation<'_> {
    fn drop(&mut self) {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(self.session_id) {
            let record = &mut inner.sessions[index];
            // Cancellation may already have advanced the live queue owner.
            // Retire our stored preparation, never a successor's slot.
            if record
                .engram
                .ordinary_handoff
                .as_ref()
                .is_some_and(|handoff| &handoff.owner == self.owner && !handoff.durable)
            {
                record.engram.ordinary_handoff = None;
            }
        }
    }
}

fn engram_ordinary_head(record: &SessionRecord) -> bool {
    record.session.parent_delegation_id.is_none()
        && record.is_local_session()
        && record.queued_prompts.front().is_some_and(|head| {
            matches!(
                head.source,
                QueuedPromptSource::User | QueuedPromptSource::Mailbox
            ) && head.engram_evaluate.as_ref().is_none_or(|evaluate| {
                evaluate.prepared_begin.is_none() && evaluate.begun_grant_id.is_none()
            })
        })
        && record.engram.active_grant_id.is_none()
        && record.engram.uncertain_grant_id.is_none()
        && record.engram.begins_recorded
        && !record.engram.checkpoint_in_progress
        && !record.engram.bind_in_progress
        && record.engram.abort_retry.is_none()
        && record.engram.bind_retry.is_none()
        && record.engram.admission_retry.is_none()
        && record.engram.retained_checkpoint.is_none()
        && record.engram.disabled_reason.is_none()
        && !record.engram.project_reset_in_progress
        && record.engram.source_observation_gate.is_none()
        && !record.engram.source_observation_delete_requested
        && !record.engram.recovered_admission
}

fn engram_ordinary_admission_candidate(record: &SessionRecord) -> bool {
    engram_ordinary_head(record)
        && !record.engram.operator_paused
        && !record.runtime_stop_in_progress
        && record.engram.begin_preparation_lineage.is_none()
        && record.queued_prompts.front().is_some_and(|head| {
            !head.has_engram_intent() && !head.engram_interrupted && !head.engram_waiting
        })
}

fn engram_ordinary_availability_cause(cause: &EngramCausalFailure) -> bool {
    cause.control_availability
        && matches!(
            cause.operation.as_str(),
            "session_bind" | "admission_binding" | "turn_evaluate"
        )
        && matches!(
            cause.failure_class,
            EngramCausalFailureClass::Transport
                | EngramCausalFailureClass::Deadline
                | EngramCausalFailureClass::Backoff
        )
        && cause.remote_application == EngramRemoteApplication::NotStarted
}

fn engram_provider_handoff_allowed(record: &SessionRecord) -> bool {
    (record.engram.active_grant_id.is_some() && engram_source_observation_can_deliver(record))
        || engram_ordinary_handoff_allowed(record)
}

fn engram_ordinary_handoff_allowed(record: &SessionRecord) -> bool {
    // An already fenced permission survives Stop arbitration. The handoff
    // caller defers it while stopping and enqueues only while Active; a fresh
    // preparation still refuses both Stop and operator pause below.
    engram_ordinary_handoff_owned(record)
        && record
            .engram
            .ordinary_handoff
            .as_ref()
            .is_some_and(|handoff| handoff.durable)
        && record
            .queued_prompts
            .front()
            .is_some_and(|head| head.engram_ordinary_handoff)
}

fn engram_ordinary_handoff_owned(record: &SessionRecord) -> bool {
    record
        .engram
        .ordinary_handoff
        .as_ref()
        .is_some_and(|handoff| handoff.owner.matches(record) && engram_ordinary_head(record))
}

impl AppState {
    fn prepare_ordinary_engram_handoff(
        &self,
        session_id: &str,
        pending: &EngramPendingDispatch,
        target: Option<&EngramBindingTarget>,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> EngramTurnDeliveryPreparation {
        let (Some(target), Some(owner), Some(cause)) =
            (target, owner, pending.causal_failure.as_ref())
        else {
            return EngramTurnDeliveryPreparation::Rejected;
        };
        let current = |inner: &StateInner, marked: bool| {
            inner.find_session_index(session_id).is_some_and(|index| {
                let record = &inner.sessions[index];
                owner.matches(record)
                    && record.session.status == SessionStatus::Active
                    && engram_ordinary_head(record)
                    && (!record.engram.operator_paused || engram_ordinary_handoff_owned(record))
                    && !record.runtime_stop_in_progress
                    && record.queued_prompts.front().is_some_and(|head| {
                        head.engram_ordinary_handoff == marked
                            && (marked || !head.engram_interrupted)
                            && head.engram_bind.as_ref().is_none_or(|bind| {
                                bind.connection == target.connection
                                    && bind.settings.same_admission_settings(&target.settings)
                            })
                            && head.engram_evaluate.as_ref().is_none_or(|evaluate| {
                                evaluate.connection == target.connection
                                    && evaluate.settings.same_admission_settings(&target.settings)
                            })
                    })
                    && Self::engram_binding_target_for_session_shape_locked(inner, session_id, true)
                        .ok()
                        .flatten()
                        .is_some_and(|now| {
                            now.connection == target.connection
                                && now.settings.same_admission_settings(&target.settings)
                        })
            })
        };
        if !engram_ordinary_availability_cause(cause) {
            return EngramTurnDeliveryPreparation::Rejected;
        }
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !current(&inner, false) {
                return EngramTurnDeliveryPreparation::Rejected;
            }
            let index = inner
                .find_session_index(session_id)
                .expect("current ordinary owner");
            if engram_ordinary_handoff_owned(&inner.sessions[index]) {
                return EngramTurnDeliveryPreparation::Superseded;
            }
            // A canceled or superseded owner cannot reserve availability for
            // the next head. The live-owner check above preserves single flight.
            inner.sessions[index].engram.ordinary_handoff = Some(EngramOrdinaryHandoff {
                owner: owner.clone(),
                durable: false,
            });
        }
        let _preparation = EngramOrdinaryPreparation {
            state: self,
            session_id,
            owner,
        };
        let mut cause = cause.clone();
        cause.continuation_reason = "Ordinary prompt sent without an Engram grant; this turn has no Engram checkpoint or verification credit.".to_owned();
        let code = match &pending.evaluated {
            EngramDispatchEvaluation::OrdinaryUnavailable { code, .. } => code.clone(),
            _ => return EngramTurnDeliveryPreparation::Rejected,
        };
        let card = EngramControlCard {
            schema_version: ENGRAM_CONTROL_SCHEMA_VERSION,
            stage: EngramControlStage::Dispatch,
            causal_failure: Some(cause),
            assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
            decision: EngramControlCardDecision::Degraded,
            dispatch: EngramControlCardDispatch::SentWithoutGrant,
            refusal_code: Some(code),
            defer_code: None,
            grant_id: None,
            directives: Vec::new(),
            delivered_range: None,
            latency_ms: EngramControlLatencyCard {
                admission_fence: pending.admission_fence_ms,
                preparation_fence: None,
                evaluate: Some(pending.evaluate_latency_ms),
                begin: None,
                checkpoint: None,
                total: duration_millis(pending.started_at.elapsed()),
            },
            fail_mode: EngramControlFailMode::Degraded,
            repair_armed: false,
            next_intent: None,
            source_root: None,
        };
        loop {
            match self.finish_engram_dispatch_record_with_defer_retry(
                session_id,
                pending.dispatch_generation,
                None,
                None,
                card.clone(),
                None,
            ) {
                EngramDispatchRecordFinish::Ready => break,
                EngramDispatchRecordFinish::DeferredByRuntimeStop => {
                    if !self.wait_for_engram_runtime_stop_resolution(
                        session_id,
                        pending.dispatch_generation,
                    ) {
                        return EngramTurnDeliveryPreparation::Rejected;
                    }
                    if !current(&self.inner.lock().expect("state mutex poisoned"), false) {
                        return EngramTurnDeliveryPreparation::Superseded;
                    }
                }
                EngramDispatchRecordFinish::PersistenceUnknown => {
                    return EngramTurnDeliveryPreparation::PersistenceUnknown;
                }
                EngramDispatchRecordFinish::Superseded => {
                    return EngramTurnDeliveryPreparation::Superseded;
                }
                _ => return EngramTurnDeliveryPreparation::Rejected,
            }
        }
        // Admission's remote budget may already have expired. This separate,
        // bounded local fence acknowledges the possible provider handoff; it
        // cannot send another control request or start an admission retry.
        let deadline =
            self.engram_budget_clock().now() + Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS);
        if self
            .confirm_engram_admission_durable_classified(session_id, deadline, owner)
            .is_err()
        {
            let _ = self.interrupt_queued_engram_admission(session_id, owner);
            return EngramTurnDeliveryPreparation::PersistenceUnknown;
        }
        loop {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let stopping = inner.find_session_index(session_id).is_some_and(|index| {
                let record = &inner.sessions[index];
                record.runtime_stop_in_progress
                    && owner.matches(record)
                    && engram_ordinary_handoff_owned(record)
            });
            if stopping {
                drop(inner);
                if !self.wait_for_engram_runtime_stop_resolution(
                    session_id,
                    pending.dispatch_generation,
                ) {
                    return EngramTurnDeliveryPreparation::Rejected;
                }
                continue;
            }
            if !current(&inner, true) {
                return EngramTurnDeliveryPreparation::Superseded;
            }
            let index = inner
                .find_session_index(session_id)
                .expect("current ordinary owner");
            let Some(handoff) = inner.sessions[index].engram.ordinary_handoff.as_mut() else {
                return EngramTurnDeliveryPreparation::Superseded;
            };
            handoff.durable = true;
            return EngramTurnDeliveryPreparation::Ready;
        }
    }
}
