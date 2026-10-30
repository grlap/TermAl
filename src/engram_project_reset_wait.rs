// The Engram project-reset waiting reason and the reset's phase timing.
//
// Owns: the preview an idle, unpaused session with queued prompts shows while
// its project's Engram reset fence holds its queue, with the preview it
// replaced saved and restored when the fence no longer holds it; and the
// phase timer and redacted read-only projection that name each step of a
// project settings reset while its fence is held.
// Does not own: the fence itself (`EngramProjectResetFences`, state.rs), the
// reset transaction (session_crud.rs), ordinary dispatch (turn_dispatch.rs),
// fair scheduling (engram_retry_schedule.rs), or process teardown.
// New file; not split out of another.

const ENGRAM_PROJECT_RESET_WAIT_PREFIX: &str = "Waiting: Engram project settings are changing";

// A controlled replay of the removed eager loops uses the same transaction
// and owned helpers. These measurements are work counts, not incident timings.
#[cfg(test)]
thread_local! {
    static TEST_ENGRAM_RESET_LEGACY_SCALE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TEST_ENGRAM_RESET_SCALED_WORK: std::cell::RefCell<BTreeMap<&'static str, usize>> =
        const { std::cell::RefCell::new(BTreeMap::new()) };
}

#[cfg(test)]
fn count_engram_reset_scaled_work(phase: &'static str, items: usize) {
    TEST_ENGRAM_RESET_SCALED_WORK
        .with(|work| *work.borrow_mut().entry(phase).or_default() += items);
}

/// One shared budget belongs to the existing fence owner, not to each RPC.
/// Local traversal assumes responsive storage and OS calls; deadline expiry
/// never certifies an original operation or a child process as settled.
#[derive(Clone)]
struct EngramProjectResetClaim {
    bounded: bool,
    generation: u64,
    claimed_at: chrono::DateTime<chrono::Utc>,
    clock: EngramBudgetClock,
    started: std::time::Instant,
    deadline: std::time::Instant,
    phase: Arc<Mutex<EngramProjectResetPhaseProgress>>,
}

struct EngramProjectResetPhaseProgress {
    sequence: u64,
    name: &'static str,
    completed: usize,
    total: usize,
    settings_committed: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramProjectResetProgress {
    bounded: bool,
    generation: u64,
    phase: &'static str,
    elapsed_ms: u64,
    remaining_ms: Option<u64>,
    deadline_expired: bool,
    completed: usize,
    total: usize,
    settings_committed: bool,
    terminal_outcome: &'static str,
}

impl EngramProjectResetClaim {
    fn refusal(&self, mut error: ApiError) -> ApiError {
        if self.bounded {
            if error.kind.is_none() {
                error.kind = Some(ApiErrorKind::EngramProjectResetRefused);
            }
            error.project_reset = Some(self.progress());
        }
        error
    }

    fn deadline_error(&self) -> ApiError {
        let mut error = ApiError::conflict(
            "The shared Engram reset budget expired before settings commit. Settings were not replaced; original operations retain their existing custody and any completed checkpoint remains completed.",
        ).with_kind(ApiErrorKind::EngramProjectResetDeadline);
        error.project_reset = Some(self.progress());
        error
    }

    fn new(generation: u64, clock: EngramBudgetClock, bounded: bool) -> Self {
        let started = clock.now();
        Self {
            bounded,
            generation,
            claimed_at: clock.wall_now(),
            clock,
            started,
            deadline: started + ENGRAM_CONTROL_SETTLE_TIMEOUT,
            phase: Arc::new(Mutex::new(EngramProjectResetPhaseProgress {
                sequence: 0,
                name: "claimed",
                completed: 0,
                total: 1,
                settings_committed: false,
            })),
        }
    }

    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(self.clock.now())
    }

    fn progress(&self) -> EngramProjectResetProgress {
        let remaining = self.remaining();
        let phase = self
            .phase
            .lock()
            .expect("project reset progress mutex poisoned");
        EngramProjectResetProgress {
            bounded: self.bounded,
            generation: self.generation,
            phase: phase.name,
            elapsed_ms: duration_millis(self.clock.elapsed_since(self.started)),
            remaining_ms: self.bounded.then(|| duration_millis(remaining)),
            deadline_expired: self.bounded && remaining.is_zero(),
            completed: phase.completed,
            total: phase.total,
            settings_committed: phase.settings_committed,
            terminal_outcome: if self.bounded {
                "applied, refused before commit, or applied with retained safety holds"
            } else {
                "legacy authority/store lifecycle; no tier-reset deadline guarantee"
            },
        }
    }
}

impl StateInner {
    fn release_reset_checkpoint_claim(&mut self, session_id: &str, generation: u64) {
        let Some(index) = self.find_session_index(session_id) else {
            return;
        };
        let live = self.sessions[index]
            .engram
            .checkpoint_operation
            .as_ref()
            .and_then(|operation| {
                self.engram_pending_checkpoint_receipts
                    .entries
                    .get(&checkpoint_receipt_key(operation))
            })
            .is_some_and(|receipt| receipt.accounting);
        if !live {
            self.session_mut_by_index(index)
                .expect("reset session")
                .engram
                .clear_checkpoint_if_owned_by(Some(generation));
        }
    }

    fn engram_project_reset_conflict(&self, project_id: &str) -> ApiError {
        let mut error = ApiError::conflict("Engram project settings are already being reset");
        error.project_reset = self
            .engram_project_resets
            .claimed_at
            .get(project_id)
            .map(EngramProjectResetClaim::progress);
        error
    }

    fn claim_engram_project_reset(&mut self, project_id: &str, bounded: bool) -> Option<u64> {
        let clock = if bounded {
            self.engram_budget_clock_snapshot()
        } else {
            EngramBudgetClock::Real
        };
        self.engram_project_resets
            .claim_with_clock(project_id, clock, bounded)
    }

    fn engram_project_reset_claim(
        &self,
        project_id: &str,
        generation: u64,
    ) -> Option<EngramProjectResetClaim> {
        self.engram_project_resets
            .claimed_at
            .get(project_id)
            .filter(|claim| claim.generation == generation)
            .cloned()
    }
}

impl AppState {
    /// Use ordinary checkpoint custody, including its persisted original
    /// request, exact receipt reservation and late-result recovery. The reset's
    /// earlier claim reserved an unsent slot, not a second wire operation.
    fn checkpoint_engram_project_reset_target(
        &self,
        target: &EngramBindingTarget,
        claim: &EngramProjectResetClaim,
        completed_absence_checkpoint: &mut Option<EngramCheckpointOperation>,
    ) -> Result<(), EngramTransportError> {
        *completed_absence_checkpoint = None;
        if claim.remaining().is_zero() {
            return Err(EngramTransportError::deadline(
                "Project reset budget exhausted before checkpoint",
            )
            .request_never_sent());
        }
        let generations = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(&target.connection.session_id) else {
                return Err(EngramTransportError::local_state(
                    "Project reset checkpoint session disappeared",
                ));
            };
            let record = &mut inner.sessions[index];
            if record.engram.checkpoint_owner_generation != Some(claim.generation)
                || record.engram.active_grant_id != target.active_grant_id
                || record.engram.routing_token != target.routing_token
            {
                return Err(EngramTransportError::local_state(
                    "Project reset checkpoint owner changed",
                ));
            }
            record
                .engram
                .clear_checkpoint_if_owned_by(Some(claim.generation));
            (record.engram.dispatch_generation, record.active_turn_generation)
        };
        let mut transport_error = None;
        let mut published_checkpoint = None;
        let result = self.checkpoint_engram_turn_with_retry_owner_off_lock(
            &target.connection.session_id,
            EngramCheckpointPurpose::Settlement,
            None,
            None,
            EngramNextIntent::Exit,
            None,
            Some(claim.generation),
            Some(claim.remaining()),
            false,
            None,
            Some(claim.deadline),
            Some(&mut published_checkpoint),
            None,
            Some(&mut transport_error),
        );
        match result {
            EngramCheckpointOutcome::Succeeded => Ok(()),
            EngramCheckpointOutcome::Failed(_) | EngramCheckpointOutcome::Skipped => {
                let error = transport_error.unwrap_or_else(|| {
                    EngramTransportError::local_state(
                        "Project reset checkpoint remains under its original recovery owner",
                    )
                });
                // This records a completed invocation, not a settled duty or
                // permission to replace its Unknown result. The ordinary
                // call has returned and released its flight/receipt guards.
                if engram_checkpoint_can_inspect_absence(&error)
                    && let Some(key) = published_checkpoint
                {
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    if let Some(operation) = inner.find_session_index(&target.connection.session_id)
                        .and_then(|index| inner.sessions[index].engram.checkpoint_operation.as_ref())
                        .and_then(|mirror| selected_checkpoint_operation(
                            &inner.engram_session_obligations, mirror))
                        .filter(|operation| checkpoint_receipt_key(operation) == key)
                        .filter(|operation| operation.attempts.last().is_some_and(|attempt|
                            (attempt.dispatch_generation, attempt.active_turn_generation) == generations))
                    {
                        *completed_absence_checkpoint = Some(operation.clone());
                    }
                }
                Err(error)
            }
        }
    }

    fn engram_project_reset_timer(
        &self,
        project_id: &str,
        generation: u64,
    ) -> EngramProjectResetTimer {
        let claim = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .engram_project_reset_claim(project_id, generation)
            .expect("owned reset claim");
        EngramProjectResetTimer::new(project_id, generation, claim)
    }

    fn engram_project_reset_progress(
        &self,
        project_id: &str,
    ) -> Result<Option<EngramProjectResetProgress>, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_project(project_id)
            .ok_or_else(|| ApiError::not_found("project not found"))?;
        Ok(inner
            .engram_project_resets
            .claimed_at
            .get(project_id)
            .map(EngramProjectResetClaim::progress))
    }
}

/// Read-only progress comes from the live fence, never from proposed settings.
async fn get_project_engram_reset(
    AxumPath(project_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let project = inner
        .find_project(&project_id)
        .ok_or_else(|| ApiError::not_found("project not found"))?;
    Ok(Json(json!({
        "reset": inner.engram_project_resets.claimed_at.get(&project_id).map(EngramProjectResetClaim::progress),
        "committed": {
            "enabled": project.engram.as_ref().is_some_and(|settings| settings.enabled),
            "turnGatedControl": project.engram.as_ref().is_some_and(|settings| settings.turn_gated_control),
        },
    })))
}

/// The waiting reason, naming when the settings change claimed its fence.
fn engram_project_reset_wait_preview(since: Option<chrono::DateTime<chrono::Utc>>) -> String {
    let since = since
        .map(|at| format!(" (since {})", at.format("%H:%M:%SZ")))
        .unwrap_or_default();
    format!(
        "{ENGRAM_PROJECT_RESET_WAIT_PREFIX}{since}; queued prompts start when the change \
         finishes. The project's Engram settings API reports the current reset phase."
    )
}

/// Every session a fence for `project_id` could hold: `session_ids`, taken
/// when it was claimed, and the project's sessions now, so a session that
/// joined while it was held is included too.
fn engram_project_reset_affected_sessions_locked(
    inner: &StateInner,
    project_id: &str,
    session_ids: &[String],
) -> Vec<String> {
    #[cfg(test)]
    count_engram_reset_scaled_work("release membership scan", inner.sessions.len());
    let mut affected = session_ids.to_vec();
    affected.extend(
        inner
            .sessions
            .iter()
            .filter(|record| {
                engram_project_for_session_locked(inner, &record.session.id)
                    .is_some_and(|project| project.id == project_id)
            })
            .map(|record| record.session.id.clone()),
    );
    affected.sort();
    affected.dedup();
    affected
}

/// Whether `record` shows the waiting reason.
fn engram_project_reset_wait_shown(record: &SessionRecord) -> bool {
    record
        .session
        .preview
        .starts_with(ENGRAM_PROJECT_RESET_WAIT_PREFIX)
}

/// Shows the waiting reason on a fenced session that would otherwise look idle
/// and ready: not in a turn, not paused (a pause shows its own reason), with
/// prompts queued. The preview it replaces is kept for the release; one with
/// no queued prompt left shows no reason. Returns whether the preview changed.
fn note_engram_project_reset_wait(
    record: &mut SessionRecord,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    if record.queued_prompts.is_empty() {
        return clear_engram_project_reset_wait(record);
    }
    let busy = matches!(
        record.session.status,
        SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
    );
    if busy || record.orchestrator_auto_dispatch_blocked {
        return false;
    }
    let preview = engram_project_reset_wait_preview(since);
    if record.session.preview == preview {
        return false;
    }
    // The preview to restore is the latest one that is not this reason; a
    // reason showing with nothing saved (another host's) restores to idle.
    if !engram_project_reset_wait_shown(record) {
        record.engram.project_reset_wait_preview = Some(record.session.preview.clone());
    } else if record.engram.project_reset_wait_preview.is_none() {
        record.engram.project_reset_wait_preview = Some("Ready for a prompt.".to_owned());
    }
    record.session.preview = preview;
    true
}

/// Removes a waiting reason this host showed, restoring the preview it
/// replaced. The saved preview marks the reason as this host's, so a reason
/// synced from another host, or a preview something else has set since, is
/// left alone. Returns whether the preview changed.
fn clear_engram_project_reset_wait(record: &mut SessionRecord) -> bool {
    let Some(saved) = record.engram.project_reset_wait_preview.take() else {
        return false;
    };
    if !engram_project_reset_wait_shown(record) {
        return false;
    }
    record.session.preview = saved;
    true
}

impl AppState {
    /// Whether a project-reset fence holds the queue of the session at `index`
    /// (its own flag, or its project's held fence). Shows the waiting reason
    /// while it does, and removes it once it does not, committing a change at
    /// a new revision so connected clients adopt it.
    fn engram_project_reset_holds_drain_locked(
        &self,
        inner: &mut StateInner,
        index: usize,
        session_id: &str,
    ) -> bool {
        let project_id =
            engram_project_for_session_locked(inner, session_id).map(|project| project.id.clone());
        let fenced = inner.sessions[index].engram.project_reset_in_progress
            || project_id
                .as_deref()
                .is_some_and(|project_id| inner.engram_project_resets.contains(project_id));
        let since = project_id
            .as_deref()
            .and_then(|project_id| inner.engram_project_resets.claimed_at(project_id));
        let record = &mut inner.sessions[index];
        let changed = if fenced {
            note_engram_project_reset_wait(record, since)
        } else {
            clear_engram_project_reset_wait(record)
        };
        if changed {
            inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            self.commit_engram_project_reset_wait_locked(inner, session_id);
        }
        fenced
    }

    /// Removes the waiting reason from each of `session_ids` the fence no
    /// longer holds, under the lock the release took. Returns whether a
    /// preview changed; the caller commits it at a new revision, or lets its
    /// own commit carry it.
    fn clear_engram_project_reset_waits_locked(
        &self,
        inner: &mut StateInner,
        session_ids: &[String],
    ) -> bool {
        let mut changed = false;
        for session_id in session_ids {
            #[cfg(test)]
            count_engram_reset_scaled_work("release preview scan", 1);
            if let Some(index) = inner.find_session_index(session_id)
                && !inner.sessions[index].engram.project_reset_in_progress
                && clear_engram_project_reset_wait(&mut inner.sessions[index])
            {
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                changed = true;
            }
        }
        changed
    }

    /// Commits a waiting-reason change. A client adopts only a newer revision,
    /// so a republished snapshot at the revision it already has would be
    /// dropped. A failed persistence attempt still consumes its revision: the
    /// change is published at it, and kept in memory for the next commit.
    fn commit_engram_project_reset_wait_locked(&self, inner: &mut StateInner, scope: &str) {
        if let Err(error) = self.commit_locked(inner) {
            eprintln!(
                "engram> {scope} failed committing the project-reset waiting reason: {error:#}"
            );
            self.publish_state_locked(inner);
        }
    }

    /// Registers ordinary service of the heads the settings change held.
    /// Sessions without a queued prompt need neither activation nor a target.
    /// Registration and the matching fence release share StateInner; the
    /// existing retry tick performs ordinary dispatch later under its cap.
    fn register_engram_project_reset_sessions_locked(
        &self,
        inner: &mut StateInner,
        session_ids: &[String],
    ) -> bool {
        let eligible_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut holds_changed = false;
        for session_id in session_ids {
            let Some(index) = inner.find_session_index(session_id) else {
                continue;
            };
            #[cfg(test)]
            count_engram_reset_scaled_work("queue registration", 1);
            if inner.sessions[index].hidden || !inner.sessions[index].is_local_session() {
                continue;
            }
            if !Self::engram_session_requires_dispatch_card_locked(inner, session_id)
                && hold_engram_queued_authorization_when_control_off(&mut inner.sessions[index])
            {
                inner.stamp_session_at_index(index);
                holds_changed = true;
            }
            let record = &mut inner.sessions[index];
            let Some(owner) = EngramQueuedAdmissionOwner::capture(record) else {
                continue;
            };
            record.engram_queue_schedule.register_activation(owner, &eligible_at);
        }
        holds_changed
    }
}

/// A preview saved while a waiting reason showed has no fence after a restart:
/// the fence is in memory only. Loading shows the idle preview instead.
fn engram_project_reset_wait_preview_after_restart(preview: String) -> String {
    if preview.starts_with(ENGRAM_PROJECT_RESET_WAIT_PREFIX) {
        "Ready for a prompt.".to_owned()
    } else {
        preview
    }
}

/// The phase timer of one project settings reset: from the fence claim to its
/// release, each step is written to host stderr when it starts and when it
/// ends, with how long the fence has been held. A step that never ends is the
/// last one started. The existing fence also holds a redacted live projection.
struct EngramProjectResetTimer {
    project_id: String,
    generation: u64,
    claim: EngramProjectResetClaim,
}

impl EngramProjectResetTimer {
    fn new(project_id: &str, generation: u64, claim: EngramProjectResetClaim) -> Self {
        let timer = Self {
            project_id: project_id.to_owned(),
            generation,
            claim,
        };
        timer.write("fence claimed".to_owned());
        timer
    }

    /// Starts the step `name` (for `session`, when it is per session); it is
    /// written as ended when the returned guard drops, early returns included.
    fn phase(&self, name: &'static str, session: Option<&str>) -> EngramProjectResetPhase<'_> {
        self.phase_work(name, session, 0, 1)
    }

    fn phase_work(
        &self,
        name: &'static str,
        session: Option<&str>,
        completed: usize,
        total: usize,
    ) -> EngramProjectResetPhase<'_> {
        let total = total.max(1);
        let completed = completed.min(total);
        let sequence = {
            let mut phase = self
                .claim
                .phase
                .lock()
                .expect("project reset progress mutex poisoned");
            phase.sequence = phase.sequence.wrapping_add(1);
            phase.name = name;
            phase.completed = completed;
            phase.total = total;
            phase.sequence
        };
        let label = match session {
            Some(session) => format!("{name} session={session}"),
            None => name.to_owned(),
        };
        self.write(format!(
            "phase {label} started (fence held {} ms)",
            self.claim
                .clock
                .elapsed_since(self.claim.started)
                .as_millis()
        ));
        EngramProjectResetPhase {
            timer: self,
            sequence,
            end_completed: completed.saturating_add(1).min(total),
            label,
            started: self.claim.clock.now(),
        }
    }

    /// Release registers exact eligible heads; it does not certify admission
    /// or delivery. Their service belongs to the existing capped scheduler.
    fn released(&self) {
        self.write(format!(
            "fence released and eligible queues registered (fence held at most {} ms)",
            self.claim
                .clock
                .elapsed_since(self.claim.started)
                .as_millis()
        ));
    }

    fn committed(&self) {
        self.claim
            .phase
            .lock()
            .expect("project reset progress mutex poisoned")
            .settings_committed = true;
    }

    fn write(&self, event: String) {
        let line = format!(
            "engram> project={} reset generation {} {event}",
            self.project_id, self.generation
        );
        eprintln!("{line}");
        #[cfg(test)]
        capture_engram_project_reset_phase_line(&self.project_id, line);
    }
}

impl Drop for EngramProjectResetTimer {
    fn drop(&mut self) {
        self.write(format!(
            "finished (fence held {} ms)",
            self.claim
                .clock
                .elapsed_since(self.claim.started)
                .as_millis()
        ));
    }
}

/// A step of a fenced transaction that holds the fence only on some paths:
/// timed when it does, nothing otherwise.
fn engram_project_reset_phase<'a>(
    timer: &'a Option<EngramProjectResetTimer>,
    name: &'static str,
    session: Option<&str>,
) -> Option<EngramProjectResetPhase<'a>> {
    timer.as_ref().map(|timer| timer.phase(name, session))
}

/// One running step of a project settings reset (`EngramProjectResetTimer`).
struct EngramProjectResetPhase<'a> {
    timer: &'a EngramProjectResetTimer,
    sequence: u64,
    end_completed: usize,
    label: String,
    started: std::time::Instant,
}

impl Drop for EngramProjectResetPhase<'_> {
    fn drop(&mut self) {
        {
            let mut phase = self
                .timer
                .claim
                .phase
                .lock()
                .expect("project reset progress mutex poisoned");
            if phase.sequence == self.sequence {
                phase.completed = self.end_completed;
            }
        }
        self.timer.write(format!(
            "phase {} took {} ms (fence held {} ms)",
            self.label,
            self.timer
                .claim
                .clock
                .elapsed_since(self.started)
                .as_millis(),
            self.timer
                .claim
                .clock
                .elapsed_since(self.timer.claim.started)
                .as_millis()
        ));
    }
}

/// Lines a test reads back, by project id; bounded per project.
#[cfg(test)]
static TEST_ENGRAM_PROJECT_RESET_PHASE_LINES: LazyLock<Mutex<HashMap<String, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
const TEST_ENGRAM_PROJECT_RESET_PHASE_LINE_LIMIT: usize = 512;

#[cfg(test)]
fn capture_engram_project_reset_phase_line(project_id: &str, line: String) {
    let mut lines = TEST_ENGRAM_PROJECT_RESET_PHASE_LINES
        .lock()
        .expect("Engram reset phase line mutex poisoned");
    let project = lines.entry(project_id.to_owned()).or_default();
    if project.len() < TEST_ENGRAM_PROJECT_RESET_PHASE_LINE_LIMIT {
        project.push(line);
    }
}

/// The phase lines written for `project_id` so far, taken.
#[cfg(test)]
fn engram_project_reset_phase_lines(project_id: &str) -> Vec<String> {
    TEST_ENGRAM_PROJECT_RESET_PHASE_LINES
        .lock()
        .expect("Engram reset phase line mutex poisoned")
        .remove(project_id)
        .unwrap_or_default()
}
