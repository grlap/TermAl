// Explicit recovery of retained source observations against their captured authority.
impl AppState {
    fn close_retired_source_capture(
        &self, session_id: &str, admission: &EngramQueuedAdmissionOwner,
        target: &EngramBindingTarget, deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        self.confirm_source_capture_recovery_durable(session_id, admission, target, deadline)?;
        let (scope, observation_id) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(session_id).ok_or_else(||
                EngramTransportError::local_state("retired source capture session was removed"))?;
            let gate = inner.sessions[index].engram.source_observation_gate.as_ref().ok_or_else(||
                EngramTransportError::local_state("retired source capture owner was removed"))?;
            let owner = inner.engram_source_sightings.iter().find(|owner| owner.scope == gate.scope)
                .ok_or_else(|| EngramTransportError::local_state("retired source capture facts were removed"))?;
            let intent = owner.observations.iter().find(|intent| intent.id == gate.observation_id
                && intent.delivery_retired).ok_or_else(||
                EngramTransportError::local_state("retired source capture intent was removed"))?;
            (owner.scope.clone(), intent.id.clone())
        };
        let intent = self.close_retired_source_history(&scope, &observation_id, target, deadline)?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id)
            && admission.matches(&inner.sessions[index])
            && inner.sessions[index].engram.routing_token.as_ref() == Some(&intent.routing_token)
            && Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .ok().flatten().is_some_and(|current| current.connection == intent.connection)
            && self.settle_engram_abort_before_handoff(&mut inner, index,
                EngramAbortReason::SourceObservation, Some(&intent.grant_id), &engram_abort_authority(target), None) {
            hold_source_observation_admission(&mut inner.sessions[index]);
            inner.stamp_session_at_index(index);
            self.commit_locked(&mut inner).map_err(|error|
                EngramTransportError::local_state(format!("retired source hold persistence failed: {error:#}")))?;
        }
        Ok(())
    }

    /// Historical grant cleanup does not borrow the successor's mutable slots.
    fn close_retired_source_history(
        &self, scope: &EngramSourceSightingScope, observation_id: &str,
        target: &EngramBindingTarget, deadline: std::time::Instant,
    ) -> Result<EngramSourceObservationIntent, EngramTransportError> {
        let (mut owner, mut intent) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let owner = inner.engram_source_sightings.iter().find(|owner| &owner.scope == scope)
                .ok_or_else(|| EngramTransportError::local_state("retired source capture facts were removed"))?;
            let intent = owner.observations.iter().find(|intent| intent.id == observation_id && intent.delivery_retired)
                .ok_or_else(|| EngramTransportError::local_state("retired source capture intent was removed"))?;
            (owner.clone(), intent.clone())
        };
        if matches!(intent.phase, EngramSourceObservationPhase::Invariant { .. }) {
            return Err(EngramTransportError::local_state("invariant source capture requires diagnosed recovery"));
        }
        let request = source_observation_settlement_request(&intent)?;
        if intent.grant_settlement_request.is_none() {
            intent.grant_settlement_request = Some(serde_json::to_value(&request).expect("settlement serializes"));
            self.replace_source_history_intent(&owner, intent.clone())?;
            owner = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner.engram_source_sightings.iter().find(|current| current.scope == owner.scope)
                    .cloned().ok_or_else(|| EngramTransportError::local_state("retired source capture owner changed"))?
            };
        }
        // The immutable measurement, original authority and close identity
        // must all be durably owned before the first possibly ambiguous RPC.
        self.confirm_source_history_durable(&owner, &target.budget_clock, deadline)?;
        if let Some(value) = &intent.grant_settlement {
            if matches!(parse_engram_result::<EngramTurnCheckpointResponse>(value.clone())?,
                EngramTurnCheckpointResponse::Checkpointed { receipt } if receipt.grant_id == intent.grant_id) {
                return Ok(intent);
            }
            return Err(EngramTransportError::protocol("retained source settlement does not match its grant"));
        }
        // Both current and superseded capture completion settle through here.
        // Delete owns the original session and its coupled marker ACK, so
        // automatic completion may preserve facts but must leave its RPC to
        // explicit Resume, which proves that whole image before recovery.
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if inner.find_session_index(&intent.session_id).is_some_and(|index|
                inner.sessions[index].engram.source_observation_delete_requested) {
                return Err(EngramTransportError::local_state(
                    "Session retained: explicit tracking recovery must acknowledge the deletion marker before settlement"));
            }
        }
        let remaining = deadline.saturating_duration_since(target.budget_clock.now());
        if remaining.is_zero() { return Err(EngramTransportError::deadline("retired source capture close budget expired")); }
        let value = target.adapter.request(&intent.connection, &request,
            remaining.min(Duration::from_millis(intent.call_timeout_ms)))?;
        if !matches!(parse_engram_result::<EngramTurnCheckpointResponse>(value.clone())?,
            EngramTurnCheckpointResponse::Checkpointed { receipt } if receipt.grant_id == intent.grant_id) {
            return Err(EngramTransportError::local_state("retired source capture has no matching grant settlement"));
        }
        intent.grant_settlement = Some(value);
        self.replace_source_history_intent(&owner, intent.clone())?;
        let saved = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner.engram_source_sightings.iter().find(|current| current.scope == owner.scope)
                .cloned().ok_or_else(|| EngramTransportError::local_state("retired source settlement owner changed"))?
        };
        self.confirm_source_history_durable(&saved, &target.budget_clock, deadline)?;
        Ok(intent)
    }

    fn confirm_source_history_durable(
        &self, owner: &EngramSourceSightingOwner, clock: &EngramBudgetClock, deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        let target = PersistFenceTarget::EngramSourceSightingHistory(Box::new(owner.clone()));
        let (fence, waiter) = PersistFence::new_with_clock(target.clone(), deadline, clock.clone());
        if self.persist_tx.send(PersistRequest::Fence(Box::new(fence))).is_ok() {
            waiter.wait().map_err(|error| EngramTransportError::deadline(format!("source recovery durability is unknown: {error:?}")))?;
        } else {
            if !engram_authority_manual_writer_allowed() {
                return Err(EngramTransportError::local_state("source recovery writer stopped; delivery remains withheld"));
            }
            #[cfg(test)]
            {
                let delta = collect_persist_delta_from_shared_state(&self.inner, 0);
                let mut cache = SqlitePersistConnectionCache::new();
                persist_delta_via_cache(&mut cache, self.persistence_path.as_path(), &delta)
                    .map_err(|error| EngramTransportError::local_state(format!("source recovery persistence failed: {error:#}")))?;
                if !cache.connection.as_ref().is_some_and(|connection| target.is_already_durable(connection).unwrap_or(false)) {
                    return Err(EngramTransportError::local_state("source recovery exact content was not committed"));
                }
            }
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        if clock.now() >= deadline || !inner.engram_source_sightings.iter().any(|current| current == owner) {
            return Err(EngramTransportError::local_state("source recovery acknowledgement owner or budget changed"));
        }
        Ok(())
    }

    fn replace_source_history_intent(
        &self, expected: &EngramSourceSightingOwner, next: EngramSourceObservationIntent,
    ) -> Result<(), EngramTransportError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let owner = inner.engram_source_sightings.iter_mut().find(|owner| *owner == expected)
            .ok_or_else(|| EngramTransportError::local_state("source recovery fact owner changed"))?;
        let intent = owner.observations.iter_mut().find(|intent| intent.id == next.id)
            .ok_or_else(|| EngramTransportError::local_state("source recovery intent was removed"))?;
        *intent = next;
        owner.version = owner.version.saturating_add(1);
        self.commit_locked(&mut inner).map_err(|error| EngramTransportError::local_state(
            format!("source recovery fact persistence failed: {error:#}")))?;
        Ok(())
    }

    /// Explicit Resume is the recovery trigger after restart or retirement.
    /// It can settle the old captured authority, never replay its provider.
    fn reconcile_retired_source_observations(&self, session_id: &str) -> Result<(), EngramTransportError> {
        self.finalize_delivered_source_observations(session_id)?;
        let clock = self.engram_budget_clock();
        let deadline = clock.now() + Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS);
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(index) = inner.find_session_index(session_id) {
                if inner.sessions[index].engram.source_observation_continuation.is_none()
                    && retire_source_observation_locked(&mut inner, index) {
                    self.commit_locked(&mut inner).map_err(|error| EngramTransportError::local_state(
                        format!("source recovery retirement persistence failed: {error:#}")))?;
                }
            }
        }
        loop {
            let pending = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner.engram_source_sightings.iter().find_map(|owner|
                    owner.observations.iter().find(|intent| intent.session_id == session_id
                        && intent.delivery_retired
                        && !matches!(intent.phase, EngramSourceObservationPhase::RefusedPolicy { .. })
                        && !intent.finalization_complete)
                    .map(|intent| (owner.clone(), intent.clone(), inner.engram_host_adapter.clone())))
            };
            let Some((owner, mut intent, adapter)) = pending else { return Ok(()); };
            self.confirm_source_history_durable(&owner, &clock, deadline)?;
            let timeout = || {
                let remaining = deadline.saturating_duration_since(clock.now());
                if remaining.is_zero() { Err(EngramTransportError::deadline("source recovery operation budget expired")) }
                else { Ok(remaining.min(Duration::from_millis(intent.call_timeout_ms))) }
            };
            match &intent.phase {
                EngramSourceObservationPhase::Invariant { reason } => return Err(
                    EngramTransportError::local_state(reason.clone())),
                EngramSourceObservationPhase::Captured => {
                    let policy = adapter.transport.read_observation_policy(&intent.connection, &owner.scope.store, timeout()?)?;
                    let status = adapter.request(&intent.connection,
                        &EngramControlRequest::SessionStatus { routing_token: intent.routing_token.clone() }, timeout()?)
                        .and_then(parse_engram_result::<EngramSessionStatusResponse>)?;
                    let observer = status.session_id.filter(|id| !id.trim().is_empty() && id.len() <= 512)
                        .ok_or_else(|| EngramTransportError::protocol("source recovery status has no producer identity"))?;
                    let observation = EngramInterTurnObservation::from_intent(&intent, policy)?;
                    intent.observing_session = Some(observer);
                    intent.phase = EngramSourceObservationPhase::Prepared { request: serde_json::to_value(
                        EngramControlRequest::ExecutionObserve { routing_token: intent.routing_token.clone(), observation })
                        .expect("validated observation serializes") };
                }
                EngramSourceObservationPhase::Prepared { request } => {
                    let wire = retained_source_observation_request(&intent, request)?;
                    let EngramControlRequest::ExecutionObserve { observation, .. } = &wire else {
                        return Err(EngramTransportError::protocol("source recovery retained another operation"));
                    };
                    let value = adapter.request(&intent.connection, &wire, timeout()?)?;
                    let observer = intent.observing_session.as_deref().ok_or_else(||
                        EngramTransportError::protocol("source recovery retained no observing session"))?;
                    let receipt = EngramSourceObservationReceipt::from_recorded_result(value, observation, observer)?;
                    intent.phase = EngramSourceObservationPhase::Recorded { request: request.clone(),
                        receipt: serde_json::to_value(receipt).expect("validated receipt serializes") };
                }
                EngramSourceObservationPhase::Recorded { request, receipt } => {
                    validate_retained_source_observation_receipt(&intent, request, receipt)?;
                    if let Some(value) = &intent.grant_settlement {
                        if !matches!(parse_engram_result::<EngramTurnCheckpointResponse>(value.clone())?,
                            EngramTurnCheckpointResponse::Checkpointed { receipt } if receipt.grant_id == intent.grant_id) {
                            return Err(EngramTransportError::protocol("retained source settlement does not match its grant"));
                        }
                        self.finalize_source_observation(&owner.scope, &intent.id, &clock, deadline)?;
                        continue;
                    } else {
                        let request = source_observation_settlement_request(&intent)?;
                        if intent.grant_settlement_request.is_none() {
                            intent.grant_settlement_request = Some(serde_json::to_value(&request).expect("settlement serializes"));
                            self.replace_source_history_intent(&owner, intent.clone())?;
                            continue; // The next iteration acknowledges the exact request before sending.
                        }
                        let value = adapter.request(&intent.connection, &request, timeout()?)?;
                        let response = parse_engram_result::<EngramTurnCheckpointResponse>(value.clone())?;
                        if !matches!(response, EngramTurnCheckpointResponse::Checkpointed { receipt }
                            if receipt.grant_id == intent.grant_id) {
                            return Err(EngramTransportError::local_state("source recovery has no matching grant settlement"));
                        }
                        intent.grant_settlement = Some(value);
                    }
                }
                EngramSourceObservationPhase::RefusedPolicy { .. } => {
                    // Its replacement owns the same grant settlement; no
                    // automatic substitute accounting is invented here.
                    return Err(EngramTransportError::local_state("source recovery policy replacement remains unresolved"));
                }
            }
            self.replace_source_history_intent(&owner, intent.clone())?;
            let saved = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner.engram_source_sightings.iter().find(|current| current.scope == owner.scope).cloned()
                    .ok_or_else(|| EngramTransportError::local_state("source recovery owner was removed"))?
            };
            self.confirm_source_history_durable(&saved, &clock, deadline)?;
        }
    }

}
