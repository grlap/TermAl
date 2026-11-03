// Ordinary observation deletion custody. Captured facts remain owned until
// their exact terminal session-row image is durable. This never retries a
// control request or prevents an ordinary turn from starting after Resume.

const SOURCE_OBSERVATION_DELETE_RETAINED: &str = "Session kept: observation reporting has not finished. Its captured facts remain retained; retry Delete after reporting completes.";
const SOURCE_OBSERVATION_ACTIVE_DELETE_RETAINED: &str = "Session kept: an observed turn bound to claimed work is still active. Stop the turn, let reporting finish, then retry Delete.";

fn source_observation_removal_content(record: &PersistedSessionRecord) -> Value {
    json!({
        "deleteRequested": record.engram_observation_delete_requested,
        "unadmittedActive": record.engram_unadmitted_active_turn,
        "unadmittedClosing": record.engram_unadmitted_closing_jobs,
        "unadmittedReports": record.engram_unadmitted_reports,
        "blocked": record.orchestrator_auto_dispatch_blocked,
    })
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
    sessions.iter().find(|session_id| {
        inner.find_session_index(session_id).is_some_and(|index| {
            let record = &inner.sessions[index];
            runtime_observation_has_removal_duty(record)
                || record.engram.source_root_bind_owner.is_some()
        })
    }).cloned()
}

fn retain_source_observation_removal_locked(inner: &mut StateInner, sessions: &[String]) {
    for session_id in sessions {
        let Some(index) = inner.find_session_index(session_id) else { continue; };
        let notice = runtime_observation_terminal_removal_notice(&inner.sessions[index]);
        let record = &mut inner.sessions[index];
        record.hidden = false;
        record.engram.observation_delete_requested = true;
        record.set_auto_dispatch_blocked(true);
        record.session.preview = notice.unwrap_or_else(|| SOURCE_OBSERVATION_DELETE_RETAINED.to_owned());
        inner.stamp_session_at_index(index);
    }
}

impl StateInner {
    fn defer_source_observation_removal(&mut self, session_id: &str) -> bool {
        if !self.delegation_inventory_complete() { return true; }
        let sessions = source_observation_removal_set(self, session_id);
        if source_observation_removal_blocker(self, &sessions).is_none() { return false; }
        retain_source_observation_removal_locked(self, &sessions);
        true
    }

    /// Runs on every general commit, so its work must not grow with the
    /// product of hidden sessions and delegations. A hidden session is
    /// retained exactly when its delegated subtree holds a removal duty; the
    /// duties are found in one pass and their ancestors in one walk, and only
    /// those hidden sessions take the per-session retention below. Retention
    /// changes visibility and dispatch markers, never the reports, jobs,
    /// traces or binding owners the duty reads, so the duty set is fixed for
    /// the whole pass and the order of hidden sessions is preserved.
    fn retain_hidden_source_observation_sessions(&mut self) {
        if !self.delegation_inventory_complete() { return; }
        let duty: Vec<&str> = self.sessions.iter()
            .filter(|record| runtime_observation_has_removal_duty(record)
                || record.engram.source_root_bind_owner.is_some())
            .map(|record| record.session.id.as_str())
            .collect();
        if duty.is_empty() { return; }
        let mut parents: HashMap<&str, Vec<&str>> = HashMap::new();
        for delegation in &self.delegations {
            parents.entry(delegation.child_session_id.as_str())
                .or_default()
                .push(delegation.parent_session_id.as_str());
        }
        let mut covering: HashSet<&str> = duty.iter().copied().collect();
        let mut frontier = duty;
        while let Some(session_id) = frontier.pop() {
            for parent in parents.get(session_id).into_iter().flatten() {
                if covering.insert(parent) { frontier.push(parent); }
            }
        }
        let hidden: Vec<String> = self.sessions.iter()
            .filter(|record| record.hidden && covering.contains(record.session.id.as_str()))
            .map(|record| record.session.id.clone())
            .collect();
        for session_id in hidden { self.defer_source_observation_removal(&session_id); }
    }
}

impl AppState {
    fn prepare_source_observation_delete(
        &self, session_id: &str, reserve_local_capture: bool,
    ) -> Result<Vec<String>, ApiError> {
        let sessions = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            inner.find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let sessions = source_observation_removal_set(&inner, session_id);
            if !reserve_local_capture && source_observation_removal_blocker(&inner, &sessions).is_none() {
                return Ok(sessions);
            }
            // Reserve the ordinary removal before off-lock runtime cleanup.
            // A concurrent Resume cancels this marker through its own owner.
            retain_source_observation_removal_locked(&mut inner, &sessions);
            self.commit_locked(&mut inner).map_err(|error| ApiError::internal(format!(
                "Session retained; observation removal persistence is uncertain: {error:#}")))?;
            sessions
        };
        for retained in &sessions { self.start_runtime_observation_worker(retained); }
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
            // Worker terminality is not a durable discharge: a settled report
            // still needs its coupled fence before its session is removed.
            if blocker.is_some() || has_runtime_custody {
                self.confirm_source_observation_removal_durable(retained)
                    .map_err(|error| ApiError::conflict(format!(
                        "Session retained; observation removal persistence is uncertain: {error}")))?;
            }
        }
        if let Some(blocker) = blocker {
            if let Some(notice) = source_observation_terminal_removal_notice(
                &self.inner.lock().expect("state mutex poisoned"), &sessions,
            ) { return Err(ApiError::conflict(notice)); }
            return Err(ApiError::conflict(format!("{SOURCE_OBSERVATION_DELETE_RETAINED} Affected session: {blocker}.")));
        }
        Ok(sessions)
    }

    fn confirm_source_observation_removal_durable(
        &self,
        session_id: &str,
    ) -> Result<(), EngramTransportError> {
        let clock = self.engram_budget_clock();
        let deadline = clock.now() + Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS);
        let content = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return Ok(());
            };
            if !inner.sessions[index]
                .engram
                .observation_delete_requested
            {
                return Err(EngramTransportError::local_state(
                    "source tracking removal owner changed",
                ));
            }
            inner.stamp_session_at_index(index);
            let content = source_observation_removal_content(&PersistedSessionRecord::from_record(
                &inner.sessions[index],
            ));
            self.commit_locked(&mut inner).map_err(|error| {
                EngramTransportError::local_state(format!(
                    "source tracking removal persistence failed: {error:#}"
                ))
            })?;
            content
        };
        let target = PersistFenceTarget::EngramObservationRemoval {
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
        {
            return Err(EngramTransportError::local_state(
                "source tracking removal owner or budget changed",
            ));
        }
        Ok(())
    }
}
