/*
Local completion of the source-observation outbox. Producer recording and
retired grant settlement do not acknowledge local cleanup. First prove the
retained owner and session projection on the same writer connection; only
then mark completion and compact payloads in a separate versioned write.
Ordinary delivered turns retain their existing lifecycle and never close here.
*/

fn source_observation_finalization_content(record: &PersistedSessionRecord) -> Value {
    json!({ "admission": engram_admission_persisted_content(record),
        "uncertainGrant": record.engram_uncertain_grant_id,
        "blocked": record.orchestrator_auto_dispatch_blocked,
        "stoppedPrompt": record.engram_stopped_prompt_id })
}

fn source_observation_same_responsibility(
    left: &EngramSourceObservationIntent, right: &EngramSourceObservationIntent,
) -> bool {
    left.session_id == right.session_id && left.prompt_id == right.prompt_id
        && left.dispatch_generation == right.dispatch_generation
        && left.active_turn_generation == right.active_turn_generation
        && left.grant_id == right.grant_id && left.binding == right.binding
        && left.connection == right.connection && left.routing_token == right.routing_token
        && left.baseline == right.baseline && left.sighting == right.sighting
        && left.root_basis == right.root_basis
}

impl AppState {
    fn finalize_source_observation(
        &self, scope: &EngramSourceSightingScope, observation_id: &str,
        clock: &EngramBudgetClock, deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        let (owner, intent, content) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let owner_index = inner.engram_source_sightings.iter().position(|owner| &owner.scope == scope)
                .ok_or_else(|| EngramTransportError::local_state("source finalization owner was removed"))?;
            let intent = inner.engram_source_sightings[owner_index].observations.iter()
                .find(|intent| intent.id == observation_id).cloned()
                .ok_or_else(|| EngramTransportError::local_state("source finalization intent was removed"))?;
            if intent.finalization_complete {
                self.compact_source_observations_locked(&mut inner)?;
                return Ok(());
            }
            let EngramSourceObservationPhase::Recorded { request, receipt } = &intent.phase else {
                return Err(EngramTransportError::local_state("source finalization still owes recording"));
            };
            let resolution = validate_retained_source_observation_receipt(&intent, request, receipt)?;
            if !intent.continuation_released || (intent.delivery_retired && intent.grant_settlement.is_none()) {
                return Err(EngramTransportError::local_state("source finalization still owes delivery transfer or settlement"));
            }
            if intent.delivery_retired && !matches!(parse_engram_result::<EngramTurnCheckpointResponse>(
                intent.grant_settlement.clone().expect("checked settlement"))?,
                EngramTurnCheckpointResponse::Checkpointed { receipt } if receipt.grant_id == intent.grant_id) {
                return Err(EngramTransportError::protocol("source finalization settlement names another grant"));
            }
            let index = inner.find_session_index(&intent.session_id)
                .ok_or_else(|| EngramTransportError::local_state("source finalization session was removed"))?;
            if inner.sessions.iter().any(|record| record.engram.source_observation_gate.as_ref()
                .is_some_and(|gate| gate.observation_id == intent.id && !gate.retired)) {
                return Err(EngramTransportError::local_state("source finalization still has a live gate"));
            }
            if resolution.is_accounted() {
                promote_engram_source_sighting(&mut inner.engram_source_sightings[owner_index], &intent.sighting);
            }
            if intent.delivery_retired {
                // A successor's authority and slots are never borrowed.
                let same_authority = Self::engram_binding_target_for_session_shape_locked(&inner, &intent.session_id, true)
                    .ok().flatten().is_some_and(|target| target.connection == intent.connection
                        && target.routing_token.as_ref() == Some(&intent.routing_token));
                let record = &mut inner.sessions[index];
                if same_authority {
                    if record.engram.active_grant_id.as_ref() == Some(&intent.grant_id) { record.engram.active_grant_id = None; }
                    if record.engram.uncertain_grant_id.as_ref() == Some(&intent.grant_id) { record.engram.uncertain_grant_id = None; }
                }
                if record.engram.source_observation_gate.as_ref().is_some_and(|gate|
                    gate.observation_id == intent.id && gate.retired) {
                    record.engram.source_observation_gate = None;
                }
            }
            // Every attempt must republish the session projection, even if
            // an earlier attempt already applied cleanup in memory. A history
            // commit may have advanced the writer watermark past its old stamp.
            inner.stamp_session_at_index(index);
            let content = source_observation_finalization_content(&PersistedSessionRecord::from_record(&inner.sessions[index]));
            let owner = inner.engram_source_sightings[owner_index].clone();
            self.commit_locked(&mut inner).map_err(|error| EngramTransportError::local_state(
                format!("source finalization persistence failed: {error:#}")))?;
            (owner, intent, content)
        };
        let target = PersistFenceTarget::EngramSourceFinalization {
            owner: Box::new(owner.clone()), session_id: intent.session_id.clone(), content: content.clone(),
        };
        let (fence, waiter) = PersistFence::new_with_clock(target.clone(), deadline, clock.clone());
        if self.persist_tx.send(PersistRequest::Fence(Box::new(fence))).is_ok() {
            waiter.wait().map_err(|error| EngramTransportError::deadline(
                format!("source finalization acknowledgement is unknown: {error:?}")))?;
        } else {
            if !engram_authority_manual_writer_allowed() {
                return Err(EngramTransportError::local_state("source finalization writer stopped"));
            }
            #[cfg(test)]
            {
                let delta = collect_persist_delta_from_shared_state(&self.inner, 0);
                let mut cache = SqlitePersistConnectionCache::new();
                persist_delta_via_cache(&mut cache, self.persistence_path.as_path(), &delta)
                    .map_err(|error| EngramTransportError::local_state(format!("source finalization write failed: {error:#}")))?;
                if !cache.connection.as_ref().is_some_and(|connection| target.is_already_durable(connection).unwrap_or(false)) {
                    return Err(EngramTransportError::local_state("source finalization exact coupled content was not committed"));
                }
            }
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&intent.session_id)
            .ok_or_else(|| EngramTransportError::local_state("source finalization session changed"))?;
        if clock.now() >= deadline || source_observation_finalization_content(
            &PersistedSessionRecord::from_record(&inner.sessions[index])) != content {
            return Err(EngramTransportError::local_state("source finalization session or budget changed"));
        }
        let current = inner.engram_source_sightings.iter_mut().find(|current| **current == owner)
            .ok_or_else(|| EngramTransportError::local_state("source finalization owner changed"))?;
        current.observations.iter_mut().find(|saved| saved.id == intent.id)
            .expect("acknowledged owner retains intent").finalization_complete = true;
        current.version = current.version.saturating_add(1);
        // This separate mutation follows the coupled ACK. Failure leaves only
        // optional compaction; it never reinstates a remote obligation.
        self.compact_source_observations_locked(&mut inner)?;
        Ok(())
    }

    fn compact_source_observations_locked(&self, inner: &mut StateInner) -> Result<(), EngramTransportError> {
        let referenced: Vec<String> = inner.sessions.iter().filter_map(|record|
            record.engram.source_observation_gate.as_ref().map(|gate| gate.observation_id.clone())).collect();
        for owner in &mut inner.engram_source_sightings {
            let unfinished: Vec<_> = owner.observations.iter().filter(|intent| !intent.finalization_complete
                && !matches!(intent.phase, EngramSourceObservationPhase::RefusedPolicy { .. })).cloned().collect();
            let completed: Vec<_> = owner.observations.iter().filter(|intent| intent.finalization_complete
                && !referenced.contains(&intent.id) && !unfinished.iter().any(|other|
                    source_observation_same_responsibility(intent, other))).cloned().collect();
            let before = owner.observations.len();
            owner.observations.retain(|intent| {
                if referenced.contains(&intent.id) { return true; }
                if completed.iter().any(|completed| completed.id == intent.id) { return false; }
                // A refused policy request is retained until its replacement
                // has finished the same responsibility, not merely recorded.
                let linked = |other: &EngramSourceObservationIntent| source_observation_same_responsibility(intent, other);
                !(matches!(intent.phase, EngramSourceObservationPhase::RefusedPolicy { .. })
                    && completed.iter().any(linked)
                    && !unfinished.iter().any(linked))
            });
            if before != owner.observations.len() { owner.version = owner.version.saturating_add(1); }
        }
        if let Err(error) = self.commit_locked(inner) {
            // Duties have already been acknowledged. Keep their completion
            // distinction even if this optional payload removal cannot queue.
            eprintln!("engram> source observation compaction remains pending: {error:#}");
        }
        Ok(())
    }

    fn finalize_delivered_source_observations(&self, session_id: &str) -> Result<(), EngramTransportError> {
        let clock = self.engram_budget_clock();
        let deadline = clock.now() + Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS);
        loop {
            let pending = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner.engram_source_sightings.iter().find_map(|owner| owner.observations.iter().find(|intent|
                    intent.session_id == session_id && !intent.delivery_retired && intent.continuation_released
                        && !intent.finalization_complete && matches!(intent.phase, EngramSourceObservationPhase::Recorded { .. }))
                    .map(|intent| (owner.scope.clone(), intent.id.clone())))
            };
            let Some((scope, id)) = pending else {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                if inner.engram_source_sightings.iter().any(|owner| owner.observations.iter()
                    .any(|intent| intent.finalization_complete)) {
                    self.compact_source_observations_locked(&mut inner)?;
                }
                return Ok(());
            };
            self.finalize_source_observation(&scope, &id, &clock, deadline)?;
        }
    }
}
