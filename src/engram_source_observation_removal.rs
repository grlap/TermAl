/*
Retains the original source-observation recovery session during removal.
Owns the common removal barrier and the deletion-specific Resume disposition.
Uses the existing retirement, immutable outbox and coupled finalization; it
does not authorize orphan recovery, provider replay or evidence abandonment.
*/

const SOURCE_OBSERVATION_DELETE_RETAINED: &str = "Session kept: source tracking has not finished. Resume recovers tracking only; then retry Delete.";
const SOURCE_OBSERVATION_ACTIVE_DELETE_RETAINED: &str = "Session kept: a runtime-started turn bound to claimed work is still active. Stop the turn, let source tracking finish, then retry Delete. Queued work remains paused.";

// Covers synchronous delivery as well as the queue worker's admission guard.
// It spans every off-lock prepare/capture boundary until its outbox is installed.
struct SourceObservationPreparationGuard<'a> {
    state: &'a AppState,
    session_id: String,
}

impl Drop for SourceObservationPreparationGuard<'_> {
    fn drop(&mut self) {
        let mut inner = self.state.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(&self.session_id) {
            let count = &mut inner.sessions[index].engram.source_observation_preparations;
            *count = count.saturating_sub(1);
        }
    }
}

fn source_observation_removal_content(record: &PersistedSessionRecord) -> Value {
    json!({ "deleteRequested": record.engram_source_observation_delete_requested,
        "admission": engram_admission_persisted_content(record),
        "unadmittedActive": record.engram_unadmitted_active_turn,
        "unadmittedClosing": record.engram_unadmitted_closing_jobs,
        "unadmittedReports": record.engram_unadmitted_reports,
        "blocked": record.orchestrator_auto_dispatch_blocked })
}

fn runtime_observation_active_removal_duty(record: &SessionRecord) -> bool {
    // Closing captures the then-current binding, not a borrowed old grant.
    // An unbound trace can never become a report at this removal cut. Keep
    // a known claimed trace even if its connection is temporarily unavailable.
    record.engram.unadmitted_active_turn.is_some()
        && record.engram.work_binding.is_some()
}

fn runtime_observation_has_removal_duty(record: &SessionRecord) -> bool {
    runtime_observation_active_removal_duty(record)
        || record.engram.unadmitted_closing_jobs.iter().any(|job| job.terminal_failure.is_none())
        || record.engram.unadmitted_reports.iter().any(|report| {
            report.envelopes.iter().any(|envelope| !matches!(envelope.delivery,
                EngramRuntimeObservationDelivery::Recorded { .. }
                    | EngramRuntimeObservationDelivery::Refused { .. }))
        })
}

fn runtime_observation_terminal_removal_notice(record: &SessionRecord) -> Option<String> {
    for report in &record.engram.unadmitted_reports {
        for envelope in &report.envelopes {
            let EngramRuntimeObservationDelivery::LocalFailure { code, .. } = &envelope.delivery else {
                continue;
            };
            // These codes and identifiers are local diagnostic facts, not a
            // reason to copy a retained transport message or request/token.
            let (code, condition) = match code.as_str() {
                "runtime_observation_retry_exhausted" => ("runtime_observation_retry_exhausted",
                    "Delivery outcome is unknown"),
                "runtime_observation_invalid_retained_image" => ("runtime_observation_invalid_retained_image",
                    "The retained observation could not be validated; any earlier delivery remains unconfirmed"),
                "runtime_observation_policy_unavailable" => ("runtime_observation_policy_unavailable",
                    "Observation policy is unavailable; any earlier delivery remains unconfirmed"),
                _ => ("runtime_observation_local_failure", "The retained observation has a local reporting failure"),
            };
            let turn = if report.host_turn_ref.len() <= 128
                && !report.host_turn_ref.is_empty()
                && report.host_turn_ref.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
                report.host_turn_ref.as_str()
            } else { "unavailable" };
            return Some(format!("Session kept: observation report for turn {turn} still needs resolution ({code}). {condition}; automatic reporting has stopped. Resume cannot resolve this report. The report is retained for support investigation before this session can be deleted."));
        }
    }
    runtime_observation_active_removal_duty(record)
        .then(|| SOURCE_OBSERVATION_ACTIVE_DELETE_RETAINED.to_owned())
}

fn source_observation_terminal_removal_notice(inner: &StateInner, sessions: &[String]) -> Option<String> {
    sessions.iter().find_map(|session| {
        let record = &inner.sessions[inner.find_session_index(session)?];
        runtime_observation_terminal_removal_notice(record)
            .map(|notice| format!("{notice} Affected session: {session}."))
    })
}

fn source_observation_unresolved(intent: &EngramSourceObservationIntent) -> bool {
    !matches!(
        intent.phase,
        EngramSourceObservationPhase::RefusedPolicy { .. }
    ) && (!matches!(intent.phase, EngramSourceObservationPhase::Recorded { .. })
        || (intent.delivery_retired && !intent.finalization_complete))
}

fn source_observation_has_removal_duty(
    owner: &EngramSourceSightingOwner,
    intent: &EngramSourceObservationIntent,
) -> bool {
    if !matches!(
        intent.phase,
        EngramSourceObservationPhase::RefusedPolicy { .. }
    ) {
        return !intent.finalization_complete;
    }
    !owner.observations.iter().any(|replacement| {
        !matches!(
            replacement.phase,
            EngramSourceObservationPhase::RefusedPolicy { .. }
        ) && source_observation_same_responsibility(intent, replacement)
            && replacement.finalization_complete
    })
}

fn source_observation_removal_set(inner: &StateInner, session_id: &str) -> Vec<String> {
    let mut sessions = vec![session_id.to_owned()];
    let mut cursor = 0;
    while cursor < sessions.len() {
        for delegation in &inner.delegations {
            if delegation.parent_session_id == sessions[cursor]
                && !sessions.contains(&delegation.child_session_id)
            {
                sessions.push(delegation.child_session_id.clone());
            }
        }
        cursor += 1;
    }
    sessions
}

fn source_observation_removal_blocker(inner: &StateInner, sessions: &[String]) -> Option<String> {
    sessions
        .iter()
        .find(|session_id| {
            inner.engram_source_sightings.iter().any(|owner| {
                owner.observations.iter().any(|intent| {
                    intent.session_id == **session_id
                        && source_observation_has_removal_duty(owner, intent)
                })
            }) || inner.find_session_index(session_id).is_some_and(|index| {
                inner.sessions[index].engram.admission_in_progress.is_some()
                    || inner.sessions[index].engram.source_observation_preparations != 0
                    || runtime_observation_has_removal_duty(&inner.sessions[index])
            })
        })
        .cloned()
}

fn retain_source_observation_removal_locked(inner: &mut StateInner, sessions: &[String]) {
    for session_id in sessions {
        let Some(index) = inner.find_session_index(session_id) else {
            continue;
        };
        retire_source_observation_locked(inner, index);
        let terminal_notice = runtime_observation_terminal_removal_notice(&inner.sessions[index]);
        let record = &mut inner.sessions[index];
        record.hidden = false;
        record.engram.source_observation_delete_requested = true;
        record.set_auto_dispatch_blocked(true);
        record.session.preview = terminal_notice.unwrap_or_else(|| SOURCE_OBSERVATION_DELETE_RETAINED.to_owned());
        inner.stamp_session_at_index(index);
    }
}

impl StateInner {
    /// Internal removals defer the whole subtree and preserve visible recovery.
    fn defer_source_observation_removal(&mut self, session_id: &str) -> bool {
        if !self.delegation_inventory_complete() { return true; }
        let sessions = source_observation_removal_set(self, session_id);
        if source_observation_removal_blocker(self, &sessions).is_none() {
            return false;
        }
        retain_source_observation_removal_locked(self, &sessions);
        eprintln!(
            "engram> session={session_id} removal deferred: source tracking requires recovery"
        );
        true
    }

    fn retain_hidden_source_observation_sessions(&mut self) {
        if !self.delegation_inventory_complete() { return; }
        let hidden: Vec<_> = self
            .sessions
            .iter()
            .filter(|record| record.hidden)
            .map(|record| record.session.id.clone())
            .collect();
        for session_id in hidden {
            self.defer_source_observation_removal(&session_id);
        }
    }
}

impl AppState {
    fn reserve_source_observation_preparation(
        &self,
        session_id: &str,
    ) -> Option<SourceObservationPreparationGuard<'_>> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(session_id)?;
        if inner.sessions[index]
            .engram
            .source_observation_delete_requested
        {
            return None;
        }
        inner.sessions[index].engram.source_observation_preparations += 1;
        Some(SourceObservationPreparationGuard {
            state: self,
            session_id: session_id.to_owned(),
        })
    }

    /// Local deletion reserves capture; proxies own no local provider capture
    /// and only retain recovery authority when an actual local duty blocks.
    fn prepare_source_observation_delete(
        &self,
        session_id: &str,
        reserve_local_capture: bool,
    ) -> Result<Vec<String>, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let sessions = source_observation_removal_set(&inner, session_id);
        if !reserve_local_capture && source_observation_removal_blocker(&inner, &sessions).is_none() {
            return Ok(sessions);
        }
        // Reserve delivery before any off-lock teardown. Even a grant still
        // capturing its opening cannot disappear or send a provider prompt.
        retain_source_observation_removal_locked(&mut inner, &sessions);
        self.commit_locked(&mut inner).map_err(|error| {
            ApiError::internal(format!(
                "Session retained; source tracking recovery persistence is uncertain: {error:#}"
            ))
        })?;
        drop(inner);
        // Delivered observations owe only local completion. Discharge it
        // while their original sessions still exist, before any teardown.
        // A failed coupled acknowledgement leaves the full duty below intact.
        for retained in &sessions {
            if let Err(error) = self.finalize_delivered_source_observations(retained) {
                eprintln!("engram> session={retained} removal retains unfinished local source completion: {error}");
            }
        }
        let blocker = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            source_observation_removal_blocker(&inner, &sessions)
        };
        for retained in &sessions {
            let has_runtime_custody = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner.find_session_index(retained).is_some_and(|index| {
                    let state = &inner.sessions[index].engram;
                    state.unadmitted_active_turn.is_some()
                        || !state.unadmitted_closing_jobs.is_empty()
                        || !state.unadmitted_reports.is_empty()
                })
            };
            // Worker terminality is not a durable discharge. In particular a
            // newly Recorded/Refused image must survive this coupled fence
            // before its session may be tombstoned.
            if blocker.is_some() || has_runtime_custody {
                self.confirm_source_observation_removal_durable(retained).map_err(|error|
                    ApiError::conflict(format!("Session retained; source tracking recovery persistence is uncertain: {error}")))?;
            }
        }
        if let Some(blocker) = blocker {
            if let Some(notice) = source_observation_terminal_removal_notice(
                &self.inner.lock().expect("state mutex poisoned"), &sessions,
            ) {
                return Err(ApiError::conflict(notice));
            }
            return Err(ApiError::conflict(format!(
                "{SOURCE_OBSERVATION_DELETE_RETAINED} Affected session: {blocker}."
            )));
        }
        Ok(sessions)
    }

    fn resume_source_observation_delete(&self, session_id: &str) -> Result<bool, ApiError> {
        let recover_binding = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            if !inner.sessions[index]
                .engram
                .source_observation_delete_requested
            {
                return Ok(false);
            }
            if inner.sessions[index].engram.admission_in_progress.is_some()
                || inner.sessions[index].engram.source_observation_preparations != 0
            {
                return Err(ApiError::conflict(
                    "Session retained: its source capture is still finishing. Retry tracking recovery after it finishes.",
                ));
            }
            if runtime_observation_active_removal_duty(&inner.sessions[index]) {
                return Err(ApiError::conflict(SOURCE_OBSERVATION_ACTIVE_DELETE_RETAINED));
            }
            let record = &inner.sessions[index];
            record.engram.rebind_required
                && !record.engram_boot_recovery_pending
                && !record.engram.bind_in_progress
                && record.engram.unadmitted_closing_jobs.iter()
                    .any(|job| job.terminal_failure.is_none())
        };
        self.confirm_source_observation_removal_durable(session_id)
            .map_err(|error| {
                ApiError::conflict(format!(
                    "Session retained; tracking recovery persistence is uncertain: {error}"
                ))
            })?;
        // Explicit tracking Resume can recover a lazy binding through the
        // existing first-use owner. It does not open an admission window,
        // evaluate/Begin a turn, or release Delete's provider-queue pause.
        if recover_binding {
            self.ensure_engram_session_bound_with_budget_off_lock(session_id, None, None)
                .map_err(|_| ApiError::conflict(
                    "Session retained: tracking binding recovery is not ready. Keep the original store and connection and retry tracking Resume; queued work remains paused.",
                ))?;
        }
        self.start_runtime_observation_worker(session_id);
        self.reconcile_retired_source_observations(session_id).map_err(|error| ApiError::conflict(format!(
            "Session retained: tracking recovery still needs its original store and connection. Queued work stays paused. {error}")))?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(blocker) = source_observation_removal_blocker(
            &inner,
            &source_observation_removal_set(&inner, session_id),
        ) {
            if let Some(notice) = source_observation_terminal_removal_notice(
                &inner, &source_observation_removal_set(&inner, session_id),
            ) {
                return Err(ApiError::conflict(notice));
            }
            return Err(ApiError::conflict(format!(
                "Session retained: source tracking still has unresolved duties in session {blocker}. Recover that session with its original store and connection; queued work remains paused."
            )));
        }
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        inner.sessions[index].set_auto_dispatch_blocked(true);
        inner.sessions[index].session.preview =
            "Source tracking recovered. Queued work remains paused; retry Delete.".to_owned();
        inner.stamp_session_at_index(index);
        self.commit_locked(&mut inner).map_err(|error| {
            ApiError::internal(format!(
                "Session retained: recovery status persistence is uncertain: {error:#}"
            ))
        })?;
        Ok(true)
    }

    fn confirm_source_observation_removal_durable(
        &self,
        session_id: &str,
    ) -> Result<(), EngramTransportError> {
        let clock = self.engram_budget_clock();
        let deadline = clock.now() + Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS);
        let (owners, content) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return Ok(());
            };
            if !inner.sessions[index]
                .engram
                .source_observation_delete_requested
            {
                return Err(EngramTransportError::local_state(
                    "source tracking removal owner changed",
                ));
            }
            inner.stamp_session_at_index(index);
            let owners = inner
                .engram_source_sightings
                .iter()
                .filter(|owner| {
                    owner
                        .observations
                        .iter()
                        .any(|intent| intent.session_id == session_id)
                })
                .cloned()
                .collect::<Vec<_>>();
            let content = source_observation_removal_content(&PersistedSessionRecord::from_record(
                &inner.sessions[index],
            ));
            self.commit_locked(&mut inner).map_err(|error| {
                EngramTransportError::local_state(format!(
                    "source tracking removal persistence failed: {error:#}"
                ))
            })?;
            (owners, content)
        };
        let target = PersistFenceTarget::EngramSourceRemoval {
            owners: owners.clone(),
            session_id: session_id.to_owned(),
            content: content.clone(),
        };
        let (fence, waiter) = PersistFence::new_with_clock(target.clone(), deadline, clock.clone());
        if self
            .persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok()
        {
            waiter.wait().map_err(|error| {
                EngramTransportError::deadline(format!(
                    "source tracking removal acknowledgement is unknown: {error:?}"
                ))
            })?;
        } else {
            if !engram_authority_manual_writer_allowed() {
                return Err(EngramTransportError::local_state(
                    "source tracking recovery writer stopped",
                ));
            }
            #[cfg(test)]
            {
                let delta = collect_persist_delta_from_shared_state(&self.inner, 0);
                let mut cache = SqlitePersistConnectionCache::new();
                persist_delta_via_cache(&mut cache, self.persistence_path.as_path(), &delta)
                    .map_err(|error| {
                        EngramTransportError::local_state(format!(
                            "source tracking recovery write failed: {error:#}"
                        ))
                    })?;
                if !cache.connection.as_ref().is_some_and(|connection| {
                    target.is_already_durable(connection).unwrap_or(false)
                }) {
                    return Err(EngramTransportError::local_state(
                        "source tracking removal coupled content was not committed",
                    ));
                }
            }
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(session_id).ok_or_else(|| {
            EngramTransportError::local_state("source tracking recovery session was removed")
        })?;
        if clock.now() >= deadline
            || source_observation_removal_content(&PersistedSessionRecord::from_record(
                &inner.sessions[index],
            )) != content
            || owners
                .iter()
                .any(|owner| !inner.engram_source_sightings.contains(owner))
        {
            return Err(EngramTransportError::local_state(
                "source tracking removal owner or budget changed",
            ));
        }
        Ok(())
    }
}
