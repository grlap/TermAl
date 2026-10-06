// Begun-grant observation barrier, bounded retry and provider continuation.
#[cfg(test)]
thread_local! {
    static TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

/// Snapshot at the same locked boundary that installs the actual measurement.
fn engram_source_observation_opening_locked(
    inner: &StateInner, index: usize, target: &EngramBindingTarget,
    admission_owner: &EngramQueuedAdmissionOwner,
) -> Option<(EngramSourceSightingScope, EngramSourceObservationIntent)> {
    let record = &inner.sessions[index];
    if !admission_owner.matches(record)
        || record.engram.active_turn_root_capture == Some(EngramRootCapture::Unconfirmed) {
        return None;
    }
    let basis = record.engram.active_turn_start_basis.clone()?;
    let observed_at = record.engram.active_turn_start_observed_at.clone()?;
    let scope = engram_source_sighting_opening_scope_locked(inner, index, &basis)?;
    let binding = record.engram.active_turn_source_binding.clone()?;
    let root_basis = record.engram.active_turn_observation_root_basis.clone().unwrap_or(Value::Null);
    let phase = if !root_basis.is_null() { EngramSourceObservationPhase::Captured } else {
        EngramSourceObservationPhase::Invariant {
            reason: "Source observation invariant failed: a measured confirmed opening has no canonical proof.".to_owned(),
        }
    };
    let plan = engram_source_opening_plan(
        inner.engram_source_sightings.iter().find(|owner| owner.scope == scope),
        &EngramSourceSighting { basis, observed_at });
    Some((scope, EngramSourceObservationIntent {
        id: Uuid::new_v4().to_string(), session_id: record.session.id.clone(),
        prompt_id: admission_owner.prompt_id.clone(), dispatch_generation: admission_owner.generation,
        active_turn_generation: admission_owner.active_turn_generation,
        grant_id: record.engram.active_grant_id.clone()?, binding,
        connection: target.connection.clone(), call_timeout_ms: duration_millis(target.settings.call_timeout()),
        routing_token: record.engram.routing_token.clone()?, observing_session: None,
        baseline: plan.baseline, sighting: plan.sighting, follow_up: plan.follow_up, root_basis, phase,
        continuation_released: true, finalization_complete: false, delivery_retired: true,
        grant_settlement: None, grant_settlement_request: None,
    }))
}

impl AppState {
    fn hold_engram_source_observation(&self, session_id: &str, owner: &EngramQueuedAdmissionOwner, reason: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else { return; };
        if inner.sessions[index].engram.source_observation_delete_requested {
            retain_source_observation_removal_locked(&mut inner, &[session_id.to_owned()]);
            if let Err(error) = self.commit_locked(&mut inner) {
                eprintln!("engram> session={session_id} retained removal persistence is unknown: {error:#}");
            }
            return;
        }
        let record = &mut inner.sessions[index];
        if !owner.matches(record) {
            if record.engram.source_observation_gate.as_ref().is_some_and(|gate|
                gate.dispatch_generation == owner.generation && gate.prompt_id == owner.prompt_id
                    && gate.active_turn_generation == owner.active_turn_generation) {
                retire_source_observation_locked(&mut inner, index);
                if let Err(error) = self.commit_locked(&mut inner) {
                    eprintln!("engram> session={session_id} source observation retirement is unknown: {error:#}");
                }
            }
            return;
        }
        record.session.status = SessionStatus::Idle;
        record.session.live_activity = None;
        record.set_auto_dispatch_blocked(true);
        record.session.preview = format!("Engram source observation withheld provider delivery: {reason}");
        if let Some(gate) = &mut record.engram.source_observation_gate {
            gate.reason = reason.to_owned();
            gate.retry_at = (chrono::Utc::now() + chrono::Duration::seconds(1))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        }
        if record.engram.source_observation_gate.as_ref().is_some_and(|gate| gate.attempts >= 8) {
            retire_source_observation_locked(&mut inner, index);
        }
        inner.stamp_session_at_index(index);
        if let Err(error) = self.commit_locked(&mut inner) {
            eprintln!("engram> session={session_id} source observation hold persistence is unknown: {error:#}");
        }
    }

    fn park_source_observation_dispatch(&self, dispatch: TurnDispatch) -> bool {
        let session_id = dispatch.session_id().to_owned();
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(&session_id) else { return false; };
        let record = &mut inner.sessions[index];
        if record.engram.source_observation_delete_requested
            || !record.runtime.matches_runtime_token(dispatch.runtime_token())
            || record.active_turn_generation != dispatch.active_turn_generation()
            || dispatch.engram_dispatch_generation() != Some(record.engram.dispatch_generation)
            || record.engram.active_grant_id.is_none() {
            return false;
        }
        record.engram.source_observation_continuation = Some(EngramSourceObservationContinuation {
            runtime: dispatch.runtime_token().clone(),
            payload: DeferredEngramHandoff(Arc::new(Mutex::new(Some(dispatch)))),
        });
        record.session.status = SessionStatus::Idle;
        record.session.live_activity = None;
        record.set_auto_dispatch_blocked(true);
        true
    }

    /// This runs inside the automatic retry tick (`engram_abort_retry_tick`,
    /// on its own thread), not on a timer or queue drain of its own.
    /// It retries accounting for the original begun grant and payload.
    fn source_observation_retry_tick(&self, now: chrono::DateTime<chrono::Utc>) {
        let (due, held_changes) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let mut due = Vec::new();
            let mut held_changes = Vec::new();
            let mut retired = false;
            for index in 0..inner.sessions.len() {
                let record = &inner.sessions[index];
                let Some(gate) = record.engram.source_observation_gate.clone() else { continue; };
                if gate.retired { continue; }
                let runtime_changed = record.engram.source_observation_continuation.as_ref()
                    .is_some_and(|continuation| !record.runtime.matches_runtime_token(&continuation.runtime));
                if !gate.owns(record) || runtime_changed || gate.attempts >= 8 {
                    held_changes.push(record.session.id.clone());
                    retired |= retire_source_observation_locked(&mut inner, index);
                    continue;
                }
                if record.engram.source_observation_continuation.is_none()
                    || record.engram.admission_in_progress.is_some()
                    || record.runtime_stop_in_progress || record.engram.project_reset_in_progress
                    || !matches!(record.session.status, SessionStatus::Idle | SessionStatus::Error)
                    || chrono::DateTime::parse_from_rfc3339(&gate.retry_at).map_or(true, |at| at > now) {
                    continue;
                }
                let session_id = record.session.id.clone();
                if engram_project_for_session_locked(&inner, &session_id)
                    .is_some_and(|project| inner.engram_project_resets.contains(&project.id)) { continue; }
                let Ok(Some(target)) = Self::engram_binding_target_for_session_shape_locked(&inner, &session_id, true) else { continue; };
                // The selected target must still be the captured connection.
                // Reset settlement uses the old authority, never this retry.
                let Some(owner) = inner.engram_source_sightings.iter().find(|owner| owner.scope == gate.scope) else { continue; };
                let Some(intent) = owner.observations.iter().find(|intent| intent.id == gate.observation_id) else { continue; };
                if target.connection != intent.connection || target.routing_token.as_ref() != Some(&intent.routing_token)
                    || target.work_binding.as_ref() != Some(&intent.binding) {
                    held_changes.push(session_id);
                    retired |= retire_source_observation_locked(&mut inner, index);
                    continue;
                }
                let wake = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let admission_owner = EngramQueuedAdmissionOwner::capture(record);
                let record = &mut inner.sessions[index];
                record.engram.admission_in_progress = Some(wake.clone());
                if let Some(gate) = &mut record.engram.source_observation_gate {
                    gate.attempts += 1;
                    gate.retry_at = (now + chrono::Duration::seconds(1_i64 << gate.attempts.min(6)))
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                }
                inner.stamp_session_at_index(index);
                due.push((session_id, target, wake, admission_owner));
            }
            if retired && let Err(error) = self.commit_locked(&mut inner) {
                // Unknown retirement persistence cannot authorize an unrelated
                // retry from this batch. Release only the guards we just took.
                for (session_id, _, wake, _) in &due {
                    if let Some(index) = inner.find_session_index(session_id)
                        && inner.sessions[index].engram.admission_in_progress.as_ref()
                            .is_some_and(|active| Arc::ptr_eq(active, wake)) {
                        inner.sessions[index].engram.admission_in_progress = None;
                    }
                }
                eprintln!("engram> source observation retirement persistence is unknown: {error:#}");
                due.clear();
            }
            (due, held_changes)
        };
        for session_id in held_changes {
            self.sync_delegation_attempt_for_child_session(&session_id);
        }
        for (session_id, target, wake, owner) in due {
            let state = self.clone();
            thread::spawn(move || {
                let guard = EngramAdmissionGuard { state: &state, session_id: &session_id, wake, owner: owner.clone() };
                let deadline = target.dispatch_deadline(target.budget_clock.now());
                match state.advance_engram_source_observation(&session_id, &target, deadline) {
                    Ok(()) => state.release_source_observation_continuation(&session_id),
                    Err(error) => if let Some(owner) = owner {
                        state.hold_engram_source_observation(&session_id, &owner, &error.to_string());
                    },
                }
                // Presentation reads admission_in_progress, so refresh only
                // after this retry releases its original admission guard.
                drop(guard);
                state.sync_delegation_attempt_for_child_session(&session_id);
            });
        }
    }

    fn release_source_observation_continuation(&self, session_id: &str) {
        let dispatch = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else { return; };
            let record = &mut inner.sessions[index];
            if !engram_source_observation_can_deliver(record) || record.runtime_stop_in_progress { return; }
            let Some(continuation) = record.engram.source_observation_continuation.as_ref() else { return; };
            if !record.runtime.matches_runtime_token(&continuation.runtime) { return; }
            let dispatch = continuation.payload.0.lock().expect("source observation continuation mutex poisoned").take();
            record.engram.source_observation_continuation = None;
            if dispatch.is_some() {
                record.session.status = SessionStatus::Active;
                record.set_auto_dispatch_blocked(false);
            }
            dispatch
        };
        if let Some(dispatch) = dispatch {
            // Reuses final Stop and runtime arbitration, without another begin.
            if let Err(error) = handoff_prepared_turn_dispatch(self, dispatch) {
                eprintln!("engram> session={session_id} resumed provider handoff failed: {error:?}");
            }
        }
    }

    /// Capture the immutable gap after the durable begin. Both boundaries are
    /// filesystem measurements, never inferred from an RPC or display window.
    fn capture_engram_source_observation(
        &self, session_id: &str, target: &EngramBindingTarget,
        admission_owner: &EngramQueuedAdmissionOwner,
        original: Option<(EngramSourceSightingScope, EngramSourceObservationIntent)>,
    ) -> Result<EngramSourceObservationCapture, EngramTransportError> {
        #[cfg(test)]
        if let Some(before) = TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| hook.borrow_mut().take()) {
            before();
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(session_id);
        if index.is_none_or(|index| !admission_owner.matches(&inner.sessions[index])) {
            if let Some((scope, intent)) = original {
                let observation_id = intent.id.clone();
                if let Some(owner) = inner.engram_source_sightings.iter_mut().find(|owner| owner.scope == scope) {
                    owner.version = owner.version.saturating_add(1);
                    owner.observations.push(intent);
                } else {
                    inner.engram_source_sightings.push(EngramSourceSightingOwner {
                        scope: scope.clone(), version: 1, latest: None, pending_close: None, observations: vec![intent],
                    });
                }
                self.commit_locked(&mut inner).map_err(|error| EngramTransportError::local_state(
                    format!("superseded source capture persistence failed: {error:#}")))?;
                return Ok(EngramSourceObservationCapture::SupersededRecovery { scope, observation_id });
            }
            let retained = inner.engram_source_sightings.iter().any(|owner|
                owner.observations.iter().any(|intent| intent.session_id == session_id
                    && intent.prompt_id == admission_owner.prompt_id
                    && intent.dispatch_generation == admission_owner.generation
                    && intent.active_turn_generation == admission_owner.active_turn_generation));
            return Ok(if retained {
                EngramSourceObservationCapture::Superseded
            } else {
                // No attributable measurement or observation state was created.
                // The lifecycle superseder owns settlement; final handoff still
                // rejects its stopped/reset runtime without creating a new close.
                EngramSourceObservationCapture::SupersededNoObservation
            });
        }
        let index = index.expect("current admission checked");
        let record = &inner.sessions[index];
        if let Some(gate) = &record.engram.source_observation_gate {
            return Ok(if gate.owns(record) { EngramSourceObservationCapture::Gate } else {
                EngramSourceObservationCapture::RetiredRecovery {
                    reason: "A retained source observation requires explicit Resume before another admission.".to_owned(),
                }
            });
        }
        if record.engram.work_binding.is_none() && record.engram.active_turn_source_binding.is_none() {
            // An unbound control turn has no claimed-work accounting scope.
            // This grants no source evidence or invented observation baseline.
            inner.sessions[index].engram.source_opening_disposition = EngramSourceOpeningDisposition::NoScope;
            inner.sessions[index].engram.source_observation_delivery_grant = inner.sessions[index].engram.active_grant_id.clone();
            return Ok(EngramSourceObservationCapture::NoEvidence);
        }
        // An existing obligation is checked before this base no-measurement
        // path. Authority loss cannot erase its already captured facts.
        let no_measurement = record.engram.active_turn_start_basis.is_none()
            || record.engram.active_turn_root_capture == Some(EngramRootCapture::Unconfirmed);
        if no_measurement {
            let binding = record.engram.active_turn_source_binding.as_ref();
            let pending = inner.engram_source_sightings.iter().any(|owner|
                binding.is_some_and(|binding| owner.scope.root_execution_id == binding.root_execution_id
                    && owner.scope.run_id == binding.run_id && owner.scope.claim_id == binding.claim_id)
                && owner.observations.iter().any(|intent|
                    !matches!(intent.phase, EngramSourceObservationPhase::Recorded { .. } | EngramSourceObservationPhase::RefusedPolicy { .. })));
            if pending {
                return Ok(EngramSourceObservationCapture::RetiredRecovery {
                    reason: "An earlier source observation is still unaccounted; explicit Resume is required.".to_owned(),
                });
            }
            let grant = record.engram.active_grant_id.clone();
            inner.sessions[index].engram.source_opening_disposition = EngramSourceOpeningDisposition::Unmeasured {
                reason: "The immutable opening has no usable attributable measurement.".to_owned(),
            };
            inner.sessions[index].engram.source_observation_delivery_grant = grant;
            return Ok(EngramSourceObservationCapture::NoEvidence);
        }
        let basis = record.engram.active_turn_start_basis.clone().expect("measured opening checked");
        let observed_at = record.engram.active_turn_start_observed_at.clone().ok_or_else(||
            EngramTransportError::local_state("opening source measurement has no timestamp"))?;
        let scope = engram_source_sighting_opening_scope_locked(&inner, index, &basis).ok_or_else(||
            EngramTransportError::local_state("opening source authority is unconfirmed"))?;
        let binding = record.engram.active_turn_source_binding.clone().ok_or_else(||
            EngramTransportError::local_state("opening source binding is unavailable"))?;
        let root_basis = record.engram.active_turn_observation_root_basis.clone().unwrap_or(Value::Null);
        let invariant_reason = root_basis.is_null().then(||
            "Source observation invariant failed: a measured confirmed opening has no canonical proof.".to_owned());
        let grant_id = record.engram.active_grant_id.clone().ok_or_else(||
            EngramTransportError::local_state("source observation has no begun grant"))?;
        let routing_token = record.engram.routing_token.clone().ok_or_else(||
            EngramTransportError::local_state("source observation has no captured routing token"))?;
        let sighting = EngramSourceSighting { basis, observed_at };
        let owner_index = inner.engram_source_sightings.iter().position(|owner| owner.scope == scope);
        let EngramSourceOpeningPlan { baseline, sighting, follow_up, equal } = engram_source_opening_plan(
            owner_index.map(|position| &inner.engram_source_sightings[position]), &sighting);
        let predecessor_pending = owner_index.is_some_and(|position| inner.engram_source_sightings[position].observations.iter()
            .any(|intent| !matches!(intent.phase, EngramSourceObservationPhase::Recorded { .. } | EngramSourceObservationPhase::RefusedPolicy { .. })));
        let recovery_reason = invariant_reason.clone().or_else(||
            record.engram.source_observation_delete_requested.then(|| SOURCE_OBSERVATION_DELETE_RETAINED.to_owned()))
            .or_else(|| predecessor_pending.then(||
            "An earlier source observation is still unaccounted; this captured grant is retired for explicit Resume.".to_owned()));
        // Equal samples prove no observed difference, not absence of ABA writes.
        if recovery_reason.is_none() && equal {
            inner.sessions[index].engram.source_opening_disposition = EngramSourceOpeningDisposition::Equal;
            inner.sessions[index].engram.source_observation_delivery_grant = Some(grant_id);
            return Ok(EngramSourceObservationCapture::NoEvidence);
        }
        let id = Uuid::new_v4().to_string();
        let intent = EngramSourceObservationIntent {
            id: id.clone(), session_id: session_id.to_owned(),
            prompt_id: admission_owner.prompt_id.clone(),
            dispatch_generation: admission_owner.generation,
            active_turn_generation: admission_owner.active_turn_generation,
            grant_id: grant_id.clone(), binding, connection: target.connection.clone(),
            call_timeout_ms: duration_millis(target.settings.call_timeout()),
            routing_token, observing_session: None, baseline, sighting, follow_up,
            root_basis, phase: invariant_reason.as_ref().map_or(EngramSourceObservationPhase::Captured,
                |reason| EngramSourceObservationPhase::Invariant { reason: reason.clone() }),
            continuation_released: recovery_reason.is_some(),
            finalization_complete: false,
            delivery_retired: recovery_reason.is_some(), grant_settlement: None,
            grant_settlement_request: None,
        };
        let owner_version = if let Some(position) = owner_index {
            let owner = &mut inner.engram_source_sightings[position];
            owner.version = owner.version.saturating_add(1);
            owner.observations.push(intent);
            owner.version
        } else {
            inner.engram_source_sightings.push(EngramSourceSightingOwner {
                scope: scope.clone(), version: 1, latest: None, pending_close: None, observations: vec![intent],
            });
            1
        };
        let record = &mut inner.sessions[index];
        record.engram.source_opening_disposition = EngramSourceOpeningDisposition::Pending;
        record.engram.source_observation_gate = Some(EngramSourceObservationGate {
            scope, observation_id: id, owner_version,
            prompt_id: admission_owner.prompt_id.clone(), dispatch_generation: admission_owner.generation,
            active_turn_generation: admission_owner.active_turn_generation, grant_id,
            attempts: 0, policy_refreshed: false,
            retry_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            reason: recovery_reason.clone().unwrap_or_else(|| "Source observation has not been accounted and acknowledged durably.".to_owned()),
            retired: recovery_reason.is_some(),
        });
        inner.stamp_session_at_index(index);
        self.commit_locked(&mut inner).map_err(|error|
            EngramTransportError::local_state(format!("source observation capture persistence failed: {error:#}")))?;
        Ok(if let Some(reason) = invariant_reason { EngramSourceObservationCapture::Invariant { reason } }
            else if let Some(reason) = recovery_reason { EngramSourceObservationCapture::RetiredRecovery { reason } }
            else { EngramSourceObservationCapture::Gate })
    }

    fn confirm_source_capture_recovery_durable(
        &self, session_id: &str, admission_owner: &EngramQueuedAdmissionOwner,
        target: &EngramBindingTarget, deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        let owner = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(session_id).ok_or_else(||
                EngramTransportError::local_state("source recovery capture session was removed"))?;
            let record = &inner.sessions[index];
            if !admission_owner.matches(record) {
                return Err(EngramTransportError::local_state("source recovery capture admission changed"));
            }
            let gate = record.engram.source_observation_gate.as_ref().ok_or_else(||
                EngramTransportError::local_state("source recovery capture has no retained owner"))?;
            inner.engram_source_sightings.iter().find(|owner| owner.scope == gate.scope
                && owner.version == gate.owner_version
                && owner.observations.iter().any(|intent| intent.id == gate.observation_id))
                .cloned().ok_or_else(|| EngramTransportError::local_state("source recovery capture fact owner changed"))?
        };
        self.confirm_source_history_durable(&owner, &target.budget_clock, deadline)?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        if target.budget_clock.now() >= deadline || inner.find_session_index(session_id)
            .is_none_or(|index| !admission_owner.matches(&inner.sessions[index])) {
            return Err(EngramTransportError::local_state("source recovery capture acknowledgement owner or budget changed"));
        }
        Ok(())
    }

    fn source_observation_snapshot(
        &self, session_id: &str,
    ) -> Result<(EngramSourceObservationGate, EngramSourceSightingOwner, EngramSourceObservationIntent), EngramTransportError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = inner.find_session_index(session_id).map(|index| &inner.sessions[index])
            .ok_or_else(|| EngramTransportError::local_state("source observation session was removed"))?;
        let gate = record.engram.source_observation_gate.as_ref().filter(|gate| gate.owns(record))
            .ok_or_else(|| EngramTransportError::local_state("source observation no longer owns its admission"))?;
        let owner = inner.engram_source_sightings.iter().find(|owner|
            owner.scope == gate.scope && owner.version == gate.owner_version)
            .ok_or_else(|| EngramTransportError::local_state("source observation fact owner changed"))?;
        let intent = owner.observations.iter().find(|intent| intent.id == gate.observation_id)
            .ok_or_else(|| EngramTransportError::local_state("source observation intent is unavailable"))?;
        Ok((gate.clone(), owner.clone(), intent.clone()))
    }

    fn confirm_source_observation_durable(
        &self, session_id: &str, gate: &EngramSourceObservationGate,
        owner: &EngramSourceSightingOwner, target: &EngramBindingTarget,
        deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        let fence_target = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(session_id).ok_or_else(||
                EngramTransportError::local_state("source observation session was removed"))?;
            let record = &inner.sessions[index];
            if !gate.owns(record) || record.engram.source_observation_gate.as_ref() != Some(gate)
                || !inner.engram_source_sightings.iter().any(|current| current == owner) {
                return Err(EngramTransportError::local_state("source observation durability owner changed"));
            }
            PersistFenceTarget::EngramSourceObservation {
                owner: Box::new(owner.clone()), session_id: session_id.to_owned(),
                admission: engram_admission_live_content(record),
            }
        };
        let (fence, waiter) = PersistFence::new_with_clock(fence_target.clone(), deadline, target.budget_clock.clone());
        if self.persist_tx.send(PersistRequest::Fence(Box::new(fence))).is_ok() {
            waiter.wait().map_err(|error| EngramTransportError::deadline(
                format!("source observation durability is unknown: {error:?}")))?;
        } else {
            if !engram_authority_manual_writer_allowed() {
                return Err(EngramTransportError::local_state("source observation writer stopped; delivery remains withheld"));
            }
            #[cfg(test)]
            {
                let delta = collect_persist_delta_from_shared_state(&self.inner, 0);
                let mut cache = SqlitePersistConnectionCache::new();
                persist_delta_via_cache(&mut cache, self.persistence_path.as_path(), &delta)
                    .map_err(|error| EngramTransportError::local_state(
                        format!("source observation persistence failed: {error:#}")))?;
                if !cache.connection.as_ref().is_some_and(|connection|
                    fence_target.is_already_durable(connection).unwrap_or(false)) {
                    return Err(EngramTransportError::local_state("source observation exact content was not committed"));
                }
            }
        }
        let (current_gate, current_owner, _) = self.source_observation_snapshot(session_id)?;
        if &current_gate != gate || &current_owner != owner || target.budget_clock.now() >= deadline {
            return Err(EngramTransportError::deadline("source observation durability did not arrive for the current owner within its budget"));
        }
        Ok(())
    }

    fn replace_source_observation_phase(
        &self, session_id: &str, expected_gate: &EngramSourceObservationGate,
        expected_owner: &EngramSourceSightingOwner, next_intent: EngramSourceObservationIntent,
    ) -> Result<(), EngramTransportError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(session_id).ok_or_else(||
            EngramTransportError::local_state("source observation session was removed"))?;
        let record = &inner.sessions[index];
        if !expected_gate.owns(record) || record.engram.source_observation_gate.as_ref() != Some(expected_gate) {
            return Err(EngramTransportError::local_state("source observation phase owner changed"));
        }
        let owner = inner.engram_source_sightings.iter_mut().find(|owner| *owner == expected_owner)
            .ok_or_else(|| EngramTransportError::local_state("source observation fact owner changed"))?;
        let intent = owner.observations.iter_mut().find(|intent| intent.id == expected_gate.observation_id)
            .ok_or_else(|| EngramTransportError::local_state("source observation intent was removed"))?;
        *intent = next_intent;
        owner.version = owner.version.saturating_add(1);
        let version = owner.version;
        inner.sessions[index].engram.source_observation_gate.as_mut().expect("checked gate").owner_version = version;
        inner.stamp_session_at_index(index);
        self.commit_locked(&mut inner).map_err(|error| EngramTransportError::local_state(
            format!("source observation phase persistence failed: {error:#}")))?;
        Ok(())
    }

    /// One bounded attempt. Captured/prepared content reaches SQLite before
    /// transport; an uncertain transport preserves exactly that wire/key.
    fn advance_engram_source_observation(
        &self, session_id: &str, target: &EngramBindingTarget, deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        loop {
            let (gate, owner, mut intent) = self.source_observation_snapshot(session_id)?;
            self.confirm_source_observation_durable(session_id, &gate, &owner, target, deadline)?;
            match &intent.phase {
                EngramSourceObservationPhase::Invariant { reason } => return Err(
                    EngramTransportError::local_state(reason.clone())),
                EngramSourceObservationPhase::Captured => {
                    let policy = target.adapter.read_observation_policy(
                        &intent.connection, &gate.scope.store, target.rpc_timeout_until(deadline)?)?;
                    let status = target.adapter.request(&intent.connection,
                        &EngramControlRequest::SessionStatus { routing_token: intent.routing_token.clone() },
                        target.rpc_timeout_until(deadline)?)
                        .and_then(parse_engram_result::<EngramSessionStatusResponse>)?;
                    if status.phase != "turn_open" || status.open_grant_id.as_ref() != Some(&intent.grant_id) {
                        return Err(EngramTransportError::local_state("source observation no longer has the captured begun grant"));
                    }
                    let observer = status.session_id.filter(|id| !id.trim().is_empty() && id.len() <= 512)
                        .ok_or_else(|| EngramTransportError::protocol("source observation status has no producer session identity"))?;
                    let observation = EngramInterTurnObservation::from_intent(&intent, policy)?;
                    intent.observing_session = Some(observer);
                    intent.phase = EngramSourceObservationPhase::Prepared {
                        request: serde_json::to_value(EngramControlRequest::ExecutionObserve {
                            routing_token: intent.routing_token.clone(), observation,
                        }).map_err(|_| EngramTransportError::protocol("source observation request could not serialize"))?,
                    };
                    self.replace_source_observation_phase(session_id, &gate, &owner, intent)?;
                }
                EngramSourceObservationPhase::Prepared { request } => {
                    let request = retained_source_observation_request(&intent, request)?;
                    let EngramControlRequest::ExecutionObserve { observation, .. } = &request else {
                        return Err(EngramTransportError::protocol("retained source observation has another operation"));
                    };
                    let value = match target.adapter.request(&intent.connection, &request, target.rpc_timeout_until(deadline)?) {
                        Ok(value) => value,
                        Err(error) if error.is_definitive_observation_policy_refusal() && !gate.policy_refreshed => {
                            // This one producer refusal proves no record exists
                            // for the old request. Preserve it and its facts;
                            // a new policy attempt receives a new identity.
                            self.refresh_refused_source_observation_policy(session_id, &gate, &owner, &intent, &error.to_string())?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let observer = intent.observing_session.as_deref().ok_or_else(||
                        EngramTransportError::protocol("retained source observation has no observing session"))?;
                    let receipt = EngramSourceObservationReceipt::from_recorded_result(value, observation, observer)?;
                    intent.phase = EngramSourceObservationPhase::Recorded {
                        request: serde_json::to_value(request).expect("decoded request serializes"),
                        receipt: serde_json::to_value(receipt).expect("decoded receipt serializes"),
                    };
                    self.replace_source_observation_phase(session_id, &gate, &owner, intent)?;
                }
                EngramSourceObservationPhase::Recorded { request, receipt } => {
                    let resolution = validate_retained_source_observation_receipt(&intent, request, receipt)?;
                    if !resolution.is_accounted() && !resolution.is_root_move() {
                        return Err(EngramTransportError::local_state(
                            "Historical binding or finished-run observation cannot release a provider."));
                    }
                    // The receipt fence above acknowledges actual accounting.
                    // Save the ordered promotion and acknowledge that owner
                    // version too before it can permit provider delivery.
                    if resolution.is_accounted()
                        && self.promote_accounted_source_observation(session_id, &gate, &owner, &intent)? {
                        continue;
                    }
                    // An owed close was reported first; the change from it to
                    // this opening is reported next, through the same gate.
                    if resolution.is_accounted() && intent.follow_up.as_ref()
                        .is_some_and(|next| next.basis != intent.sighting.basis) {
                        self.chain_source_observation_follow_up(session_id, &gate, &owner, &intent)?;
                        continue;
                    }
                    // The preceding fence includes the exact receipt and queue
                    // owner. Time and ownership are rechecked at release too.
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    let index = inner.find_session_index(session_id).ok_or_else(||
                        EngramTransportError::local_state("source observation session was removed"))?;
                    let record = &mut inner.sessions[index];
                    if !gate.owns(record) || record.engram.source_observation_gate.as_ref() != Some(&gate)
                        || target.budget_clock.now() >= deadline {
                        return Err(EngramTransportError::local_state("source observation release owner or budget changed"));
                    }
                    record.engram.source_opening_disposition = if resolution.is_accounted() {
                        EngramSourceOpeningDisposition::Accounted { observation_id: intent.id.clone() }
                    } else {
                        EngramSourceOpeningDisposition::HistoricalRootMove { observation_id: intent.id.clone() }
                    };
                    record.engram.source_observation_delivery_grant = Some(gate.grant_id.clone());
                    return Ok(());
                }
                EngramSourceObservationPhase::RefusedPolicy { .. } => return Err(
                    EngramTransportError::local_state("retired source observation cannot release a continuation")),
            }
        }
    }

    fn promote_accounted_source_observation(
        &self, session_id: &str, gate: &EngramSourceObservationGate,
        expected: &EngramSourceSightingOwner, intent: &EngramSourceObservationIntent,
    ) -> Result<bool, EngramTransportError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(session_id).ok_or_else(||
            EngramTransportError::local_state("source promotion session was removed"))?;
        if !gate.owns(&inner.sessions[index]) || inner.sessions[index].engram.source_observation_gate.as_ref() != Some(gate) {
            return Err(EngramTransportError::local_state("source promotion admission owner changed"));
        }
        let owner = inner.engram_source_sightings.iter_mut().find(|owner| *owner == expected)
            .ok_or_else(|| EngramTransportError::local_state("source promotion fact owner changed"))?;
        // What Engram recorded settles the duty it covers, whether or not a
        // later measurement keeps it from becoming the baseline.
        let settled = settle_engram_pending_close_by_observation(owner, &intent.sighting, true);
        if !promote_engram_source_sighting(owner, &intent.sighting) && !settled { return Ok(false); }
        let version = owner.version;
        inner.sessions[index].engram.source_observation_gate.as_mut().expect("checked gate").owner_version = version;
        inner.stamp_session_at_index(index);
        self.commit_locked(&mut inner).map_err(|error|
            EngramTransportError::local_state(format!("source promotion persistence failed: {error:#}")))?;
        Ok(true)
    }

    /// Replaces the gate's recorded observation of an owed close with the
    /// observation of the change from that close to the opening, as the
    /// policy refresh below replaces a refused one.
    fn chain_source_observation_follow_up(
        &self, session_id: &str, gate: &EngramSourceObservationGate,
        expected_owner: &EngramSourceSightingOwner, intent: &EngramSourceObservationIntent,
    ) -> Result<(), EngramTransportError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(session_id).ok_or_else(||
            EngramTransportError::local_state("source observation session was removed"))?;
        if !gate.owns(&inner.sessions[index]) || inner.sessions[index].engram.source_observation_gate.as_ref() != Some(gate) {
            return Err(EngramTransportError::local_state("source follow-up owner changed"));
        }
        let owner = inner.engram_source_sightings.iter_mut().find(|owner| *owner == expected_owner)
            .ok_or_else(|| EngramTransportError::local_state("source follow-up fact owner changed"))?;
        let previous = owner.observations.iter_mut().find(|previous| previous.id == intent.id)
            .ok_or_else(|| EngramTransportError::local_state("source follow-up intent was removed"))?;
        let Some(follow_up) = previous.follow_up.take() else {
            return Err(EngramTransportError::local_state("source follow-up has no next sighting"));
        };
        previous.continuation_released = true;
        let mut next = intent.clone();
        next.id = Uuid::new_v4().to_string();
        next.observing_session = None;
        next.phase = EngramSourceObservationPhase::Captured;
        next.baseline = engram_follow_up_baseline(&intent.sighting, &follow_up);
        next.sighting = follow_up;
        next.follow_up = None;
        let next_id = next.id.clone();
        owner.observations.push(next);
        owner.version = owner.version.saturating_add(1);
        let version = owner.version;
        let gate = inner.sessions[index].engram.source_observation_gate.as_mut().expect("checked gate");
        gate.observation_id = next_id;
        gate.owner_version = version;
        // The follow-up is a new observation with its own one policy refresh.
        gate.policy_refreshed = false;
        inner.stamp_session_at_index(index);
        self.commit_locked(&mut inner).map_err(|error| EngramTransportError::local_state(
            format!("source follow-up persistence failed: {error:#}")))?;
        Ok(())
    }

    fn refresh_refused_source_observation_policy(
        &self, session_id: &str, gate: &EngramSourceObservationGate,
        expected_owner: &EngramSourceSightingOwner, intent: &EngramSourceObservationIntent, reason: &str,
    ) -> Result<(), EngramTransportError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(session_id).ok_or_else(||
            EngramTransportError::local_state("source observation session was removed"))?;
        if !gate.owns(&inner.sessions[index]) || inner.sessions[index].engram.source_observation_gate.as_ref() != Some(gate) {
            return Err(EngramTransportError::local_state("source policy refresh owner changed"));
        }
        let owner = inner.engram_source_sightings.iter_mut().find(|owner| *owner == expected_owner)
            .ok_or_else(|| EngramTransportError::local_state("source policy refresh fact owner changed"))?;
        let previous = owner.observations.iter_mut().find(|previous| previous.id == intent.id)
            .ok_or_else(|| EngramTransportError::local_state("source policy refresh intent was removed"))?;
        let EngramSourceObservationPhase::Prepared { request } = &previous.phase else {
            return Err(EngramTransportError::local_state("source policy refresh has no refused request"));
        };
        previous.phase = EngramSourceObservationPhase::RefusedPolicy { request: request.clone(), reason: reason.to_owned() };
        previous.continuation_released = true;
        let mut next = intent.clone();
        next.id = Uuid::new_v4().to_string();
        next.observing_session = None;
        next.phase = EngramSourceObservationPhase::Captured;
        let next_id = next.id.clone();
        owner.observations.push(next);
        owner.version = owner.version.saturating_add(1);
        let version = owner.version;
        let gate = inner.sessions[index].engram.source_observation_gate.as_mut().expect("checked gate");
        gate.observation_id = next_id;
        gate.owner_version = version;
        gate.policy_refreshed = true;
        inner.stamp_session_at_index(index);
        self.commit_locked(&mut inner).map_err(|error| EngramTransportError::local_state(
            format!("source policy refresh persistence failed: {error:#}")))?;
        Ok(())
    }
}
