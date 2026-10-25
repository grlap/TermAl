// The Engram project-reset waiting reason and the reset's phase timing.
//
// Owns: the preview an idle, unpaused session with queued prompts shows while
// its project's Engram reset fence holds its queue, with the preview it
// replaced saved and restored when the fence no longer holds it; and the
// phase timer that writes each step of a project settings reset to host
// stderr, so a reset that keeps its fence names the step it is in.
// Does not own: the fence itself (`EngramProjectResetFences`, state.rs), the
// reset transaction (session_crud.rs), the queue drain (turn_dispatch.rs), the
// order the release drains sessions in, or process teardown.
// New file; not split out of another.

const ENGRAM_PROJECT_RESET_WAIT_PREFIX: &str = "Waiting: Engram project settings are changing";

/// The waiting reason, naming when the settings change claimed its fence.
fn engram_project_reset_wait_preview(since: Option<chrono::DateTime<chrono::Utc>>) -> String {
    let since = since
        .map(|at| format!(" (since {})", at.format("%H:%M:%SZ")))
        .unwrap_or_default();
    format!(
        "{ENGRAM_PROJECT_RESET_WAIT_PREFIX}{since}; queued prompts start when the change \
         finishes. If it does not, the host log's lines for this project show how far it got."
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

    /// Drains the queues of `session_ids` once no fence holds them, so what a
    /// settings change held starts when it ends, however it ends. Only a
    /// session with a queued prompt is drained: with none there is nothing to
    /// start, and a drain would still prepare Engram context for nothing.
    /// Call with the state lock released.
    fn drain_engram_project_reset_sessions(&self, session_ids: Vec<String>, after: &str) {
        let session_ids = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            session_ids
                .into_iter()
                .filter(|session_id| {
                    inner
                        .find_session_index(session_id)
                        .is_some_and(|index| !inner.sessions[index].queued_prompts.is_empty())
                })
                .collect::<Vec<_>>()
        };
        for session_id in session_ids {
            match self.dispatch_next_queued_turn(&session_id, false) {
                Ok(Some(dispatch)) => {
                    if let Err(error) = deliver_turn_dispatch(self, dispatch)
                        .into_background_result("session activation queue drain")
                    {
                        eprintln!(
                            "engram> session={session_id} failed delivering queued turn after {after}: {}",
                            error.message
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => eprintln!(
                    "engram> session={session_id} failed resuming queue after {after}: {error:#}"
                ),
            }
        }
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
/// last one started. Nothing is kept beyond the lines themselves.
struct EngramProjectResetTimer {
    project_id: String,
    generation: u64,
    claimed: std::time::Instant,
}

impl EngramProjectResetTimer {
    fn new(project_id: &str, generation: u64) -> Self {
        let timer = Self {
            project_id: project_id.to_owned(),
            generation,
            claimed: std::time::Instant::now(),
        };
        timer.write("fence claimed".to_owned());
        timer
    }

    /// Starts the step `name` (for `session`, when it is per session); it is
    /// written as ended when the returned guard drops, early returns included.
    fn phase(&self, name: &'static str, session: Option<&str>) -> EngramProjectResetPhase<'_> {
        let label = match session {
            Some(session) => format!("{name} session={session}"),
            None => name.to_owned(),
        };
        self.write(format!(
            "phase {label} started (fence held {} ms)",
            self.claimed.elapsed().as_millis()
        ));
        EngramProjectResetPhase {
            timer: self,
            label,
            started: std::time::Instant::now(),
        }
    }

    /// Writes that the release returned. The release removes the fence first
    /// and then drains the queues it held, so the fence was held at most this
    /// long; the drain's own time is in the `release and drain` phase.
    fn released(&self) {
        self.write(format!(
            "fence released and queues drained (fence held at most {} ms)",
            self.claimed.elapsed().as_millis()
        ));
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
            self.claimed.elapsed().as_millis()
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
    label: String,
    started: std::time::Instant,
}

impl Drop for EngramProjectResetPhase<'_> {
    fn drop(&mut self) {
        self.timer.write(format!(
            "phase {} took {} ms (fence held {} ms)",
            self.label,
            self.started.elapsed().as_millis(),
            self.timer.claimed.elapsed().as_millis()
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
