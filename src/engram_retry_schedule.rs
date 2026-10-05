// The schedule every automatic Engram admission retry shares. Owns the delays
// and their jitter (`engram_abort_retry_delay`), the durable acknowledgement
// a retry record waits for before anything is admitted
// (`request_engram_abort_acknowledgement_locked`,
// `engram_retry_acknowledgement_step`), the one-attempt-per-head guard
// (`engram_retry_attempt_in_flight`), the host-wide cap on attempts in flight
// (`EngramRetrySlots`), and the tick that orders due attempts by due time and
// starts them (`engram_abort_retry_tick`), on its own thread
// (`spawn_engram_retry_tick`). Does not own any retry's record,
// eligibility, release check or dispatch: the abort retry of a withheld
// delivery (`engram_abort_retry.rs`), the retained-bind retry
// (`engram_bind_retry.rs`) and the parked-admission retry
// (`engram_admission_retry.rs`) each own theirs. Split out of
// `engram_abort_retry.rs`, where the delays and the tick began, when a third
// retry would otherwise have copied them.

/// The delay before the next automatic attempt, by the number of attempts so
/// far; the last value repeats.
const ENGRAM_ABORT_RETRY_DELAYS_SECONDS: [u64; 6] = [2, 5, 10, 20, 30, 60];

/// The largest positive jitter added to a retry delay, in percent.
const ENGRAM_ABORT_RETRY_MAX_JITTER_PERCENT: u64 = 20;

/// How long one acknowledgement of a retry record may take before the tick
/// asks again. A later request has its own deadline; the original
/// admission's dispatch budget is never stretched.
const ENGRAM_ABORT_SETTLEMENT_FENCE: Duration = Duration::from_secs(20);

/// The most in flight at once, host-wide: the automatic attempts of every
/// retry kind and each parked admission's boot reconciliation (an abort
/// retry's boot recovery keeps its existing path outside the cap). Bounds the
/// herd after a restart or an Engram outage; control-process concurrency is
/// per session.
const ENGRAM_RETRY_MAX_IN_FLIGHT: usize = 4;

/// How often the retry tick runs.
#[cfg(not(test))]
const ENGRAM_RETRY_TICK: Duration = Duration::from_secs(2);

impl AppState {
    /// Starts the automatic retry tick on a thread of its own, so the
    /// attempts it runs (each bounded by the admission budget) never hold up
    /// carried-gate polling or the test-run index. It stops once shutdown is
    /// signalled. Tests drive the tick directly with their own clock and
    /// never start this thread.
    #[cfg(not(test))]
    fn spawn_engram_retry_tick(&self) {
        let state = self.clone();
        let shutdown = self.subscribe_shutdown_signal();
        let spawned = std::thread::Builder::new()
            .name("termal-engram-retry".to_owned())
            .spawn(move || {
                while !*shutdown.borrow() {
                    state.engram_abort_retry_tick(chrono::Utc::now());
                    std::thread::sleep(ENGRAM_RETRY_TICK);
                }
            });
        if let Err(err) = spawned {
            eprintln!("engram> failed to start the automatic retry tick: {err}");
        }
    }
}

/// The pending durable acknowledgement of a retry record, kept on the session
/// for the tick to look at without blocking.
#[derive(Clone)]
struct EngramAbortAckWaiter(Arc<PersistFenceWaiter>);

impl std::fmt::Debug for EngramAbortAckWaiter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EngramAbortAckWaiter")
    }
}

/// The delay before attempt `attempts + 1`: the schedule's value plus up to
/// `ENGRAM_ABORT_RETRY_MAX_JITTER_PERCENT`, spread by session and attempt so
/// sessions held together do not retry together.
fn engram_abort_retry_delay(session_id: &str, attempts: u32) -> chrono::Duration {
    let index = usize::try_from(attempts.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(ENGRAM_ABORT_RETRY_DELAYS_SECONDS.len() - 1);
    let base_ms = ENGRAM_ABORT_RETRY_DELAYS_SECONDS[index] * 1000;
    let digest = sha256_hex(format!("{session_id}:{attempts}").as_bytes());
    let spread = u64::from_str_radix(&digest[..8], 16).unwrap_or(0)
        % (ENGRAM_ABORT_RETRY_MAX_JITTER_PERCENT + 1);
    let jittered = base_ms * (100 + spread) / 100;
    chrono::Duration::milliseconds(i64::try_from(jittered).unwrap_or(i64::MAX))
}

/// What one tick does with a session's retry record.
#[derive(Debug, PartialEq, Eq)]
enum EngramAbortRetryStep {
    /// Nothing yet: the acknowledgement is pending or the retry not due.
    Wait,
    /// The record no longer applies and was dropped; the hold stays.
    Dropped,
    /// The record needs a (new) durable acknowledgement.
    Acknowledge,
    /// The record was acknowledged just now; the prompt waits for its retry.
    Acknowledged,
    /// The attempt is due. The hold stays: the attempt bypasses it only for
    /// this exact head, revalidated under the promotion lock.
    Due,
}

/// The one-attempt-per-head guard: an admission of this session's head is
/// running (a retry's own attempt, an explicit Resume or an ordinary drain),
/// so no retry starts another one; whatever that admission ends in decides.
fn engram_retry_attempt_in_flight(record: &SessionRecord) -> bool {
    record.engram.admission_in_progress.is_some() || record.engram.pending_dispatch.is_some()
}

/// The acknowledgement part of a retry step: `None` once the session's retry
/// record is durably acknowledged, else the step to take now. When the
/// acknowledgement has just arrived, `acknowledge` marks the kind's own
/// record so the saved record says so.
fn engram_retry_acknowledgement_step(
    record: &mut SessionRecord,
    acknowledge: impl FnOnce(&mut SessionRecord),
) -> Option<EngramAbortRetryStep> {
    if record.engram.abort_retry_acknowledged {
        return None;
    }
    let saved = if record.engram.abort_retry_saved {
        Some(Ok(()))
    } else {
        record
            .engram
            .abort_retry_fence
            .as_ref()
            .map(|waiter| waiter.0.wait_until(std::time::Instant::now()))
            .unwrap_or(Some(Err(PersistFenceError::Deadline)))
    };
    Some(match saved {
        // Still being written: look again next tick.
        None => EngramAbortRetryStep::Wait,
        // Never asked, or the worker gave up: ask again.
        Some(Err(_)) => EngramAbortRetryStep::Acknowledge,
        Some(Ok(())) => {
            record.engram.abort_retry_acknowledged = true;
            record.engram.abort_retry_saved = false;
            record.engram.abort_retry_fence = None;
            acknowledge(record);
            EngramAbortRetryStep::Acknowledged
        }
    })
}

/// Whether an RFC 3339 due time has passed at `now`; an unreadable one has.
fn engram_retry_due_passed(due_at: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(due_at)
        .map(|due| due.with_timezone(&chrono::Utc))
        .unwrap_or(now)
        <= now
}

/// Which retry a session's record is for. At most one is set; the order
/// only matters for a record a bug left beside another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramRetryKind {
    Bind,
    Admission,
    Abort,
}

fn engram_retry_kind(record: &SessionRecord) -> Option<EngramRetryKind> {
    if record.engram.bind_retry.is_some() {
        Some(EngramRetryKind::Bind)
    } else if record.engram.admission_retry.is_some() {
        Some(EngramRetryKind::Admission)
    } else if record.engram.abort_retry.is_some() {
        Some(EngramRetryKind::Abort)
    } else {
        None
    }
}

/// The due time of `kind`'s record on `record`, for ordering due attempts.
fn engram_retry_due_at(record: &SessionRecord, kind: EngramRetryKind) -> String {
    match kind {
        EngramRetryKind::Bind => record.engram.bind_retry.as_ref().map(|retry| retry.due_at.clone()),
        EngramRetryKind::Admission => record
            .engram
            .admission_retry
            .as_ref()
            .map(|retry| retry.due_at.clone()),
        EngramRetryKind::Abort => record.engram.abort_retry.as_ref().map(|retry| retry.due_at.clone()),
    }
    .unwrap_or_default()
}

/// The host-wide count of automatic attempts in flight, capped at
/// `ENGRAM_RETRY_MAX_IN_FLIGHT`, and how many due attempts the last tick had
/// to defer for want of a slot. Shared by every clone of the state. Both are
/// lock-free atomics: a slot may be taken with or without the state mutex
/// held, and the pool itself takes no lock, so it cannot invert with it.
///
/// It also holds every automatic attempt back while the host boots: from
/// construction, before the tick's thread starts, until boot preparation has
/// raised each restarted session's readiness fence, so a retry rebuilt on
/// load is never replayed ahead of that session's boot reconciliation.
#[derive(Clone, Default)]
struct EngramRetrySlots {
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
    deferred_due: Arc<std::sync::atomic::AtomicUsize>,
    boot_pending: Arc<std::sync::atomic::AtomicBool>,
}

/// One attempt's place under the cap, released when the attempt completes,
/// however it ends (a delivery, a park, a timeout or a failure to start).
struct EngramRetrySlot(Arc<std::sync::atomic::AtomicUsize>);

/// `EngramRetrySlots::release_boot_hold_on_drop`'s guard.
struct EngramRetryBootHoldRelease(Arc<std::sync::atomic::AtomicBool>);

impl Drop for EngramRetryBootHoldRelease {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl EngramRetrySlots {
    fn try_acquire(&self) -> Option<EngramRetrySlot> {
        self.in_flight
            .try_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |in_flight| (in_flight < ENGRAM_RETRY_MAX_IN_FLIGHT).then_some(in_flight + 1),
            )
            .ok()
            .map(|_| EngramRetrySlot(self.in_flight.clone()))
    }

    /// A slot for a call outside the tick (a drain, a Resume, a rebind) on a
    /// head the schedule owns. It never jumps ahead of due attempts the tick
    /// already deferred for want of a slot: while any wait, it takes none.
    fn try_acquire_opportunistic(&self) -> Option<EngramRetrySlot> {
        if self.deferred_due.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            return None;
        }
        self.try_acquire()
    }

    fn record_deferred_due(&self, deferred: usize) {
        self.deferred_due
            .store(deferred, std::sync::atomic::Ordering::SeqCst);
    }

    /// Holds every automatic attempt back until boot preparation releases it
    /// (`release_boot_hold_on_drop`).
    fn hold_until_boot_preparation(&self) {
        self.boot_pending
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn boot_preparation_pending(&self) -> bool {
        self.boot_pending.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Releases the boot hold when dropped, so boot preparation releases it
    /// on every exit: success, an error (a failed commit) or a panic. A held
    /// hold would stop every automatic retry for the life of the process.
    fn release_boot_hold_on_drop(&self) -> EngramRetryBootHoldRelease {
        EngramRetryBootHoldRelease(self.boot_pending.clone())
    }

    #[cfg(test)]
    fn in_flight(&self) -> usize {
        self.in_flight.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for EngramRetrySlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A due attempt the tick found: the session, the exact head it is due for,
/// which retry it is and when it was due.
struct EngramDueRetry {
    session_id: String,
    owner: EngramQueuedAdmissionOwner,
    kind: EngramRetryKind,
    due_at: String,
}

impl AppState {
    /// Asks the persistence worker to acknowledge, against the saved record,
    /// the admission content `index`'s session holds now, retry record
    /// included, and keeps the waiter for the tick.
    fn request_engram_abort_acknowledgement_locked(&self, inner: &mut StateInner, index: usize) {
        let record = &mut inner.sessions[index];
        let (fence, waiter) = PersistFence::new(
            PersistFenceTarget::EngramAdmission {
                session_id: record.session.id.clone(),
                content: engram_admission_live_content(record),
            },
            std::time::Instant::now() + ENGRAM_ABORT_SETTLEMENT_FENCE,
        );
        if self
            .persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok()
        {
            record.engram.abort_retry_fence = Some(EngramAbortAckWaiter(Arc::new(waiter)));
            return;
        }
        // Shutdown or a test without a worker: a synchronous save of the same
        // content is the acknowledgement, as the admission fence falls back
        // to it.
        record.engram.abort_retry_fence = None;
        let saved = self.persist_internal_locked(inner);
        let record = &mut inner.sessions[index];
        match saved {
            Ok(()) => record.engram.abort_retry_saved = true,
            Err(error) => eprintln!(
                "engram> session={} failed saving the retry record: {error:#}",
                record.session.id
            ),
        }
    }

    /// One pass over every session with a retry record at `now`: drops a
    /// record that no longer applies, asks for a missing or failed durable
    /// acknowledgement, releases an acknowledged prompt for its retry, and
    /// starts the attempts that are due, earliest due first, as many as the
    /// host-wide cap has room for. An attempt the cap defers is logged and
    /// left as it is: its attempt index and first-held time do not move, and
    /// a later tick starts it. Driven by its own thread
    /// (`spawn_engram_retry_tick`); tests call it with their own clock.
    fn engram_abort_retry_tick(&self, now: chrono::DateTime<chrono::Utc>) {
        self.source_observation_retry_tick(now);
        // Before boot preparation has raised the restarted sessions'
        // readiness fences, a retry rebuilt on load would be replayed without
        // its reconciliation: start nothing until then.
        if self.engram_retry_slots.boot_preparation_pending() {
            return;
        }
        let budget_now = self.engram_budget_clock().now();
        let mut due = Vec::new();
        // Sessions whose hold changed: a held delegation child reports it.
        let mut held_changes = Vec::new();
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let session_ids = inner
                .sessions
                .iter()
                .filter(|record| engram_retry_kind(record).is_some())
                .map(|record| record.session.id.clone())
                .collect::<Vec<_>>();
            let mut changed = false;
            for session_id in session_ids {
                let Some(index) = inner.find_session_index(&session_id) else {
                    continue;
                };
                // A settings transaction holds the project's fence while it
                // validates off the lock, and may still roll back: the
                // binding target reads as unavailable meanwhile. Wait for
                // the committed authority rather than read that as a change.
                let fenced = inner.sessions[index].engram.project_reset_in_progress
                    || engram_project_for_session_locked(&inner, &session_id)
                        .is_some_and(|project| inner.engram_project_resets.contains(&project.id));
                if fenced {
                    continue;
                }
                let authority =
                    Self::engram_binding_target_for_session_shape_locked(&inner, &session_id, true)
                        .ok()
                        .flatten()
                        .map(|target| engram_abort_authority(&target));
                let Some(kind) = engram_retry_kind(&inner.sessions[index]) else {
                    continue;
                };
                let record = &mut inner.sessions[index];
                let step = match kind {
                    EngramRetryKind::Bind => {
                        engram_bind_retry_step(record, authority.as_deref(), now, budget_now)
                    }
                    EngramRetryKind::Admission => {
                        engram_admission_retry_step(record, authority.as_deref(), now, budget_now)
                    }
                    EngramRetryKind::Abort => {
                        engram_abort_retry_step(record, authority.as_deref(), now)
                    }
                };
                match step {
                    EngramAbortRetryStep::Wait => {}
                    EngramAbortRetryStep::Acknowledge => {
                        self.request_engram_abort_acknowledgement_locked(&mut inner, index);
                    }
                    EngramAbortRetryStep::Dropped | EngramAbortRetryStep::Acknowledged => {
                        inner.stamp_session_at_index(index);
                        changed = true;
                        held_changes.push(session_id);
                    }
                    EngramAbortRetryStep::Due => {
                        if engram_source_observation_holds_admission(&inner, index) {
                            if hold_source_observation_admission(&mut inner.sessions[index]) {
                                inner.stamp_session_at_index(index);
                                changed = true;
                                held_changes.push(session_id);
                            }
                            continue;
                        }
                        // The attempt bypasses the paused queue only for the
                        // head it was due for, revalidated under the
                        // promotion lock; a Cancel, takeover or changed
                        // authority in between starts nothing.
                        if let Some(owner) =
                            EngramQueuedAdmissionOwner::capture(&inner.sessions[index])
                        {
                            let due_at = engram_retry_due_at(&inner.sessions[index], kind);
                            due.push(EngramDueRetry {
                                session_id,
                                owner,
                                kind,
                                due_at,
                            });
                        }
                    }
                }
            }
            if changed && let Err(error) = self.commit_locked(&mut inner) {
                eprintln!("engram> failed persisting the retry tick: {error:#}");
            }
        }
        for session_id in held_changes {
            self.sync_delegation_attempt_for_child_session(&session_id);
        }
        // Earliest due first; RFC 3339 times written by this host in UTC with
        // millisecond precision order as strings.
        due.sort_by(|left, right| left.due_at.cmp(&right.due_at));
        let mut admitted = Vec::new();
        let mut deferred = 0;
        for retry in due {
            match self.engram_retry_slots.try_acquire() {
                Some(slot) => admitted.push((retry, slot)),
                None => {
                    deferred += 1;
                    eprintln!(
                        "engram> session={} automatic {:?} retry due at {} deferred: {} attempts \
                         already in flight host-wide",
                        retry.session_id, retry.kind, retry.due_at, ENGRAM_RETRY_MAX_IN_FLIGHT
                    );
                }
            }
        }
        // Calls outside the tick wait behind these (`try_acquire_opportunistic`).
        self.engram_retry_slots.record_deferred_due(deferred);
        if admitted.len() == 1 {
            let (retry, slot) = admitted.pop().expect("one admitted attempt");
            self.run_engram_retry_attempt(retry, now, slot);
        } else if !admitted.is_empty() {
            self.run_engram_retry_attempts_concurrently(admitted, now);
        }
    }

    /// Runs two or more admitted attempts side by side and returns when all
    /// have completed. An attempt whose thread cannot be created runs on the
    /// tick's thread instead. Each attempt carries its own panic boundary
    /// (`run_engram_retry_attempt`); a worker that still ends in a panic is
    /// logged when joined, so the scope never resumes it on the tick's thread.
    fn run_engram_retry_attempts_concurrently(
        &self,
        admitted: Vec<(EngramDueRetry, EngramRetrySlot)>,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            let mut inline = Vec::new();
            for (retry, slot) in admitted {
                let session_id = retry.session_id.clone();
                let attempt = (retry, slot);
                #[cfg(test)]
                if engram_retry_test_spawn_fails(self) {
                    inline.push(attempt);
                    continue;
                }
                // The closure owns the attempt only once the thread exists:
                // a failed spawn hands it back through the shared cell.
                let cell = Arc::new(Mutex::new(Some(attempt)));
                let worker_cell = cell.clone();
                match std::thread::Builder::new()
                    .name(format!("engram-retry-{session_id}"))
                    .spawn_scoped(scope, move || {
                        let attempt = worker_cell
                            .lock()
                            .expect("retry attempt cell poisoned")
                            .take();
                        if let Some((retry, slot)) = attempt {
                            self.run_engram_retry_attempt(retry, now, slot);
                        }
                    }) {
                    Ok(worker) => workers.push((session_id, worker)),
                    Err(error) => {
                        eprintln!(
                            "engram> session={session_id} automatic retry thread unavailable ({error}); running it on the tick's thread"
                        );
                        if let Some(attempt) =
                            cell.lock().expect("retry attempt cell poisoned").take()
                        {
                            inline.push(attempt);
                        }
                    }
                }
            }
            for (retry, slot) in inline {
                self.run_engram_retry_attempt(retry, now, slot);
            }
            for (session_id, worker) in workers {
                if worker.join().is_err() {
                    eprintln!(
                        "engram> session={session_id} automatic retry attempt panicked; its record is kept for a later tick"
                    );
                }
            }
        });
    }

    /// Runs one due attempt behind a panic boundary, on whichever thread
    /// runs it (a worker, the tick's thread for a single attempt, or the
    /// tick's thread when no worker could be created): the tick's thread
    /// serves every session's retries, so no one attempt may take it down.
    /// A panicked attempt is logged, its record kept for a later tick, and
    /// its slot released by unwinding.
    fn run_engram_retry_attempt(
        &self,
        retry: EngramDueRetry,
        now: chrono::DateTime<chrono::Utc>,
        slot: EngramRetrySlot,
    ) {
        let session_id = retry.session_id.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            self.run_engram_retry_attempt_unguarded(retry, now, slot)
        }));
        if outcome.is_err() {
            eprintln!(
                "engram> session={session_id} automatic retry attempt panicked; its record is kept for a later tick"
            );
        }
    }

    /// Starts one due attempt and delivers what it admits. `slot` is its
    /// place under the cap, released on return. A parked admission whose
    /// session still has a restart's readiness fence raised starts that
    /// session's one lazy boot recovery instead, hands it the slot until the
    /// recovery finishes, and is not charged. An
    /// attempt that failed before admission counts as an attempt and moves to
    /// its next delay; a parked admission the drain declined (another
    /// admission holds the head, or something now holds it that the next
    /// step reads) is not charged, by cause rather than by a later read.
    fn run_engram_retry_attempt_unguarded(
        &self,
        retry: EngramDueRetry,
        now: chrono::DateTime<chrono::Utc>,
        slot: EngramRetrySlot,
    ) {
        let EngramDueRetry {
            session_id,
            owner,
            kind,
            ..
        } = retry;
        #[cfg(test)]
        engram_retry_test_panic_point(self, &session_id);
        if kind == EngramRetryKind::Admission {
            let fenced = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner
                    .find_session_index(&session_id)
                    .is_some_and(|index| inner.sessions[index].engram_boot_recovery_pending)
            };
            if fenced {
                // The recovery holds this attempt's slot until it finishes.
                self.request_engram_boot_recovery_retry_holding(&session_id, Some(slot));
                return;
            }
        }
        let retry_owner = owner.clone();
        let dispatch = match kind {
            EngramRetryKind::Bind => self.dispatch_next_queued_turn_for_bind_retry(&session_id, owner),
            EngramRetryKind::Admission => {
                self.dispatch_next_queued_turn_for_admission_retry(&session_id, owner)
            }
            EngramRetryKind::Abort => self.dispatch_next_queued_turn_for_abort_retry(&session_id, owner),
        };
        let (started, failed) = match dispatch {
            Ok(Some(dispatch)) => {
                // The slot follows the delivery, onto the Fast discovery
                // worker when delivery moves there.
                if let Err(error) = deliver_turn_dispatch_holding(self, dispatch, Some(slot))
                    .into_background_result("engram abort retry")
                {
                    eprintln!(
                        "engram> session={session_id} failed delivering the retried prompt: {}",
                        error.message
                    );
                }
                (true, false)
            }
            Ok(None) => (false, false),
            Err(error) => {
                eprintln!(
                    "engram> session={session_id} failed preparing the retried prompt: {error:#}"
                );
                (false, true)
            }
        };
        if !started {
            match kind {
                EngramRetryKind::Bind => self.postpone_engram_bind_retry(&session_id, &retry_owner, now),
                EngramRetryKind::Admission => {
                    if failed {
                        self.postpone_engram_admission_retry(&session_id, &retry_owner, now);
                    }
                }
                EngramRetryKind::Abort => {
                    self.postpone_engram_abort_retry(&session_id, &retry_owner.prompt_id, now)
                }
            }
        }
    }
}

/// Test faults for the concurrent branch, keyed by the state they apply to,
/// so parallel tests never see each other's.
#[cfg(test)]
#[derive(Default)]
struct EngramRetryTestFaults {
    spawn_fails: bool,
    panic_session: Option<String>,
}

#[cfg(test)]
static ENGRAM_RETRY_TEST_FAULTS: LazyLock<Mutex<HashMap<usize, EngramRetryTestFaults>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Installs `faults` for `state` until the returned guard is dropped, so a
/// test that panics never leaves them for a later state at the same address.
#[cfg(test)]
fn set_engram_retry_test_faults(
    state: &AppState,
    faults: EngramRetryTestFaults,
) -> EngramRetryTestFaultsGuard {
    let key = Arc::as_ptr(&state.inner) as usize;
    ENGRAM_RETRY_TEST_FAULTS.lock().unwrap().insert(key, faults);
    EngramRetryTestFaultsGuard(key)
}

#[cfg(test)]
struct EngramRetryTestFaultsGuard(usize);

#[cfg(test)]
impl Drop for EngramRetryTestFaultsGuard {
    fn drop(&mut self) {
        if let Ok(mut faults) = ENGRAM_RETRY_TEST_FAULTS.lock() {
            faults.remove(&self.0);
        }
    }
}

#[cfg(test)]
fn engram_retry_test_spawn_fails(state: &AppState) -> bool {
    ENGRAM_RETRY_TEST_FAULTS
        .lock()
        .unwrap()
        .get(&(Arc::as_ptr(&state.inner) as usize))
        .is_some_and(|faults| faults.spawn_fails)
}

#[cfg(test)]
fn engram_retry_test_panic_point(state: &AppState, session_id: &str) {
    let panics = ENGRAM_RETRY_TEST_FAULTS
        .lock()
        .unwrap()
        .get(&(Arc::as_ptr(&state.inner) as usize))
        .is_some_and(|faults| faults.panic_session.as_deref() == Some(session_id));
    if panics {
        panic!("injected automatic retry attempt panic for {session_id}");
    }
}
