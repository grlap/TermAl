// Observation delivery runs independently of runtime completion. Integration
// supplies EngramSessionState.unadmitted_reports (persisted) and
// unadmitted_worker_running (runtime-only, defaults false on restore).
// Neither field participates in an admission/recovery predicate.

enum EngramRuntimeObservationWork {
    Capture(EngramRuntimeClosingJob),
    Deliver(EngramRuntimeObservationReport),
}

// An unavailable producer never blocks later historical reports forever.
// Exhaustion retains the immutable wire as a visible local failure.
const ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS: u32 = 8;

// Process-local retirement barrier. A worker registers before checking the
// stop flag and holds its guard through CAS and persistence, including the
// synchronous fallback. Restore may start only after this registry and the
// persistence thread have both drained.
#[derive(Default)]
struct EngramRuntimeObservationLifetime {
    stopping: std::sync::atomic::AtomicBool,
    installs: Mutex<usize>,
    drained: std::sync::Condvar,
    #[cfg(test)]
    install_hook: Mutex<Option<Arc<dyn Fn(&'static str) + Send + Sync>>>,
}

struct EngramRuntimeObservationInstall {
    lifetime: Arc<EngramRuntimeObservationLifetime>,
}

impl Drop for EngramRuntimeObservationInstall {
    fn drop(&mut self) {
        let mut count = self
            .lifetime
            .installs
            .lock()
            .expect("observation install counter poisoned");
        *count -= 1;
        if *count == 0 {
            self.lifetime.drained.notify_all();
        }
    }
}

impl EngramRuntimeObservationLifetime {
    fn enter(self: &Arc<Self>, phase: &'static str) -> Option<EngramRuntimeObservationInstall> {
        let mut count = self
            .installs
            .lock()
            .expect("observation install counter poisoned");
        *count += 1;
        let guard = EngramRuntimeObservationInstall {
            lifetime: self.clone(),
        };
        drop(count);
        if self.stopping.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        #[cfg(test)]
        self.observe(phase);
        #[cfg(not(test))]
        let _ = phase;
        Some(guard)
    }

    fn stop_and_join(&self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        #[cfg(test)]
        self.observe("stopping");
        let mut count = self
            .installs
            .lock()
            .expect("observation install counter poisoned");
        while *count != 0 {
            count = self
                .drained
                .wait(count)
                .expect("observation install counter poisoned");
        }
    }

    #[cfg(test)]
    fn observe(&self, phase: &'static str) {
        let hook = self.install_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(phase);
        }
    }
}

impl EngramRuntimeObservationWork {
    fn due(&self) -> &str {
        match self {
            Self::Capture(job) => &job.retry_at,
            Self::Deliver(report) => &report.retry_at,
        }
    }
}

impl AppState {
    // Reuse the binding/readiness owner's process-local fences. Restore does
    // not persist the work binding, so its absence before rebind is not proof
    // that the immutable closing scope changed. Delivery needs no current bind.
    fn runtime_observation_capture_waits_for_binding(record: &SessionRecord) -> bool {
        record.engram_boot_recovery_pending
            || record.engram.rebind_required
            || record.engram.bind_in_progress
    }

    // Called after closing capture and by post-listen boot for restored
    // reports. Repeated calls coalesce under the state lock. A turn callback
    // never joins this worker or waits for a producer/persistence reply.
    fn start_runtime_observation_worker(&self, session_id: &str) {
        if self
            .runtime_observation_lifetime
            .stopping
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let state = &mut inner.sessions[index].engram;
            if state.unadmitted_worker_running
                || state
                    .unadmitted_reports
                    .iter()
                    .all(|report| report.is_settled())
                    && state
                        .unadmitted_closing_jobs
                        .iter()
                        .all(|job| job.terminal_failure.is_some())
            {
                return;
            }
            state.unadmitted_worker_running = true;
        }
        let state = self.clone();
        let worker_session = session_id.to_owned();
        let spawned = std::thread::Builder::new()
            .name("termal-unadmitted-observation".to_owned())
            .spawn(move || state.run_runtime_observation_worker(&worker_session));
        if let Err(error) = spawned {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(index) = inner.find_session_index(session_id) {
                inner.sessions[index].engram.unadmitted_worker_running = false;
            }
            eprintln!("engram> observation worker for {session_id} did not start: {error}");
        }
    }

    fn run_runtime_observation_worker(&self, session_id: &str) {
        let shutdown = self.subscribe_shutdown_signal();
        while !*shutdown.borrow()
            && !self
                .runtime_observation_lifetime
                .stopping
                .load(std::sync::atomic::Ordering::Acquire)
        {
            let candidate = {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                let Some(index) = inner.find_session_index(session_id) else {
                    return;
                };
                let adapter = inner.engram_host_adapter.clone();
                let capture_waits = Self::runtime_observation_capture_waits_for_binding(
                    &inner.sessions[index],
                );
                let state = &mut inner.sessions[index].engram;
                let pending = state
                    .unadmitted_reports
                    .iter()
                    .filter(|report| !report.is_settled())
                    .min_by_key(|report| report.retry_at.clone())
                    .cloned()
                    .map(EngramRuntimeObservationWork::Deliver);
                let closing = state
                    .unadmitted_closing_jobs
                    .iter()
                    .filter(|job| !capture_waits && job.terminal_failure.is_none())
                    .min_by_key(|job| job.retry_at.clone())
                    .cloned()
                    .map(EngramRuntimeObservationWork::Capture);
                let pending = pending
                    .into_iter()
                    .chain(closing)
                    .min_by_key(|work| work.due().to_owned());
                let Some(work) = pending else {
                    state.unadmitted_worker_running = false;
                    #[cfg(test)]
                    self.runtime_observation_lifetime.observe("idle");
                    return;
                };
                (adapter, work)
            };
            let (adapter, work) = candidate;
            if !engram_retry_due_passed(work.due(), chrono::Utc::now()) {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            if let EngramRuntimeObservationWork::Capture(job) = work {
                // In particular the policy-pin phase must reach SQLite before
                // any filesystem measurement uses it, including after restart.
                if !runtime_observation_session_entry_is_durable(
                    &self.persistence_path,
                    session_id,
                    "engramUnadmittedClosingJobs",
                    &serde_json::to_value(&job).expect("closing job serializes"),
                )
                .unwrap_or(false)
                {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                let Some(_slot) = self.engram_retry_slots.try_acquire_opportunistic() else {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                };
                let result = self.capture_runtime_observation_report(session_id, &job);
                if *shutdown.borrow() {
                    break;
                }
                if let Err(error) = self.install_runtime_closing_result(session_id, &job, result) {
                    eprintln!("engram> runtime closing capture was not saved: {error:#}");
                }
                continue;
            }
            let EngramRuntimeObservationWork::Deliver(report) = work else {
                unreachable!();
            };
            // No serialized request can leave before its exact session-row
            // image exists. This wait is private to delivery, not the lane.
            match runtime_observation_report_is_durable(&self.persistence_path, session_id, &report)
            {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            }
            let Some(_slot) = self.engram_retry_slots.try_acquire_opportunistic() else {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            };
            if *shutdown.borrow() {
                break;
            }
            let Some(envelope_index) = report.pending() else {
                continue;
            };
            let timeout = Duration::from_millis(report.call_timeout_ms);
            let next =
                deliver_runtime_observation_envelope(&adapter, &report, envelope_index, timeout);
            // A reply arriving after this process was shut down cannot
            // overwrite a restarted host's pending image. That host replays
            // the retained request and validates its own immutable receipt.
            if *shutdown.borrow() {
                break;
            }
            let mut next = match next {
                Ok(next) => next,
                Err(error) => {
                    let mut failed = report.next_version();
                    failed.envelopes[envelope_index].delivery =
                        EngramRuntimeObservationDelivery::LocalFailure {
                            code: "runtime_observation_invalid_retained_image".to_owned(),
                            message: error.message,
                        };
                    failed
                }
            };
            let failed = matches!(
                next.envelopes[envelope_index].delivery,
                EngramRuntimeObservationDelivery::Pending { .. }
            );
            if failed {
                next.attempts = next.attempts.saturating_add(1);
                if next.attempts >= ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS {
                    let message = match &next.envelopes[envelope_index].delivery {
                        EngramRuntimeObservationDelivery::Pending { last_error } => last_error
                            .clone()
                            .unwrap_or_else(|| "delivery remained uncertain".to_owned()),
                        _ => unreachable!(),
                    };
                    next.envelopes[envelope_index].delivery =
                        EngramRuntimeObservationDelivery::LocalFailure {
                            code: "runtime_observation_retry_exhausted".to_owned(),
                            message,
                        };
                    next.attempts = 0;
                }
                next.retry_at = (chrono::Utc::now()
                    + engram_abort_retry_delay(session_id, next.attempts))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            } else {
                next.attempts = 0;
                next.retry_at = unadmitted_observation_timestamp();
            }
            if let Err(error) =
                self.install_runtime_observation_result(session_id, &report, next, envelope_index)
            {
                eprintln!(
                    "engram> observation settlement for {} is not saved: {error:#}",
                    report.host_turn_ref
                );
            }
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id) {
            inner.sessions[index].engram.unadmitted_worker_running = false;
        }
    }

    fn install_runtime_closing_result(
        &self,
        session_id: &str,
        expected: &EngramRuntimeClosingJob,
        result: Result<EngramRuntimeClosingOutcome, EngramTransportError>,
    ) -> Result<()> {
        let Some(_install) = self.runtime_observation_lifetime.enter("capture") else {
            return Ok(());
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(());
        };
        // Recovery may have started while off-lock capture was in flight.
        // Its completion re-kicks this worker; do not spend an attempt or
        // install a stale policy/report/error while that fence is raised.
        if Self::runtime_observation_capture_waits_for_binding(&inner.sessions[index]) {
            return Ok(());
        }
        let state = &mut inner.sessions[index].engram;
        let Some(position) = state
            .unadmitted_closing_jobs
            .iter()
            .position(|job| job == expected)
        else {
            return Ok(());
        };
        let mut diagnostic = None;
        match result {
            Ok(EngramRuntimeClosingOutcome::BindingRecoveryPending) => return Ok(()),
            Ok(EngramRuntimeClosingOutcome::Policy(policy)) => {
                let job = &mut state.unadmitted_closing_jobs[position];
                job.policy = Some(policy);
                job.attempts = 0;
                job.retry_at = unadmitted_observation_timestamp();
                job.last_error = None;
            }
            Ok(EngramRuntimeClosingOutcome::Report(report)) => {
                if !state
                    .unadmitted_reports
                    .iter()
                    .any(|existing| existing.host_turn_ref == report.host_turn_ref)
                {
                    state.unadmitted_reports.push(report);
                }
                state.unadmitted_closing_jobs.remove(position);
            }
            Err(error) => {
                let failed = expected.failed(session_id, &error);
                if error.code.as_deref() == Some("runtime_observation_scope_changed")
                    || failed.attempts >= ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS
                {
                    diagnostic = Some(format!(
                        "Runtime-started turn {} not reported to Engram: {}. Its checks remain uncredited.",
                        expected.trace.host_turn_ref, error.message
                    ));
                    let mut failed = failed;
                    failed.terminal_failure = Some(error.message);
                    state.unadmitted_closing_jobs[position] = failed;
                } else {
                    state.unadmitted_closing_jobs[position] = failed;
                }
            }
        }
        let has_diagnostic = diagnostic.is_some();
        if let Some(text) = diagnostic {
            let message = Message::Text {
                attachments: Vec::new(),
                id: inner.next_message_id(),
                timestamp: stamp_now(),
                author: Author::System,
                text,
                expanded_text: None,
                source: None,
            };
            push_message_on_record(&mut inner.sessions[index], message);
        }
        inner.stamp_session_at_index(index);
        if has_diagnostic {
            self.commit_locked(&mut inner)?;
        } else {
            self.persist_internal_locked(&inner)?;
        }
        drop(inner);
        #[cfg(test)]
        self.runtime_observation_lifetime.observe("closing_installed");
        Ok(())
    }

    // Only this immutable historical entry changes. A successor runtime,
    // current binding, queue, grant and source-sighting baseline are not
    // upgraded by a successful old receipt. Refusal diagnostics are inserted
    // with the settlement under the same lock and leave the current preview.
    fn install_runtime_observation_result(
        &self,
        session_id: &str,
        expected: &EngramRuntimeObservationReport,
        next: EngramRuntimeObservationReport,
        envelope_index: usize,
    ) -> Result<()> {
        let Some(_install) = self.runtime_observation_lifetime.enter("delivery") else {
            return Ok(());
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(());
        };
        let Some(position) = inner.sessions[index]
            .engram
            .unadmitted_reports
            .iter()
            .position(|report| report == expected)
        else {
            return Ok(());
        };
        let diagnostic = match &next.envelopes[envelope_index].delivery {
            EngramRuntimeObservationDelivery::Refused { code, .. } => Some(format!(
                "Engram refused the observation of the runtime-started turn ({code}). Its checks remain uncredited."
            )),
            EngramRuntimeObservationDelivery::LocalFailure { code, message } => Some(format!(
                "Runtime-started turn not reported to Engram ({code}): {message}. Its checks remain uncredited."
            )),
            _ => None,
        };
        inner.sessions[index].engram.unadmitted_reports[position] = next;
        let message = diagnostic.map(|text| Message::Text {
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            author: Author::System,
            text,
            expanded_text: None,
            source: None,
        });
        let event = message.map(|message| {
            let record = &mut inner.sessions[index];
            let message_index = push_message_on_record(record, message.clone());
            let session_seq = record.next_body_delta_seq(message.id());
            (
                message,
                global_message_index(record, message_index),
                session_message_count(record),
                record.session.preview.clone(),
                record.session.status,
                record.mutation_stamp,
                session_seq,
            )
        });
        inner.stamp_session_at_index(index);
        if let Some((message, message_index, message_count, preview, status, stamp, session_seq)) =
            event
        {
            let revision = self.commit_persisted_delta_locked(&mut inner)?;
            self.publish_delta_locked(
                &inner,
                DeltaEvent::MessageCreated {
                    revision,
                    session_id: session_id.to_owned(),
                    message_id: message.id().to_owned(),
                    message_index,
                    message_count,
                    message,
                    preview,
                    status,
                    session_queue: None,
                    session_mutation_stamp: Some(stamp),
                    session_seq: Some(session_seq),
                    body_seq_epoch: Some(self.server_instance_id.clone()),
                },
            );
        } else {
            self.persist_internal_locked(&inner)?;
        }
        Ok(())
    }

    fn resume_runtime_observation_reports(&self) {
        let sessions = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter(|record| {
                    record
                        .engram
                        .unadmitted_closing_jobs
                        .iter()
                        .any(|job| job.terminal_failure.is_none())
                        || record
                            .engram
                            .unadmitted_reports
                            .iter()
                            .any(|report| !report.is_settled())
                })
                .map(|record| record.session.id.clone())
                .collect::<Vec<_>>()
        };
        for session in sessions {
            self.start_runtime_observation_worker(&session);
        }
    }
}
