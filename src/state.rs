/*
State and persistence core
                 +------------------------+
REST / runtimes ->| AppState              |-> stream_events (state + delta, in order)
remote bridge  -> | - coordination shell  |-> file_events
                  | - remote registry      |
                  | - shared Codex runtime |
                  +-----------+------------+
                              |
                              v
                       +------+------+
                       | StateInner  |
                       | projects    |
                       | sessions    |
                       | orchestrators
                       | workspaces  |
                       +------+------+
                              |
                              v
                    ~/.termal/termal.sqlite
AppState owns live coordination primitives that should not be serialized.
StateInner is the durable model plus counters and indexes protected by one
mutex.
*/

use super::*;

/// Wake signal sent to the background persist thread.
///
/// The persist thread owns an `Arc<StateMutex<StateInner>>` and collects
/// the diff itself on each tick — it captures a lightweight selection under
/// the global mutex, then clones each selected session/delegation in a separate
/// acquisition before writing to SQLite via `persist_delta_via_cache` (see
/// `persist.rs`).
/// Ordinary requests carry only a wake signal. Fences additionally carry a
/// narrow content target and a completion handle, never a full state snapshot.
pub(crate) enum PersistRequest {
    /// Incremental persist: the thread looks up the current
    /// `last_mutation_stamp` and writes only the sessions that
    /// advanced past the thread's own watermark.
    Delta,
    /// A content-specific durability acknowledgement. Not yet used by request
    /// handlers; ordinary state writes retain their asynchronous contract.
    #[allow(dead_code)]
    Fence(Box<PersistFence>),
    /// Graceful-shutdown signal: the persist worker performs one final
    /// drain-and-write tick (so any pending mutation reaches SQLite),
    /// then exits its loop. The matching `JoinHandle` lives on
    /// `AppState::persist_thread_handle` and `AppState::shutdown_persist_blocking`
    /// is the documented shutdown entry point — see also bugs.md
    /// "Server restart without browser refresh can lose the last
    /// streamed message" for the durability contract this closes.
    Shutdown,
}

/// The one process-wide durable-state mutex, with production diagnostics for
/// the rare long hold that otherwise presents as several unrelated HTTP
/// requests completing at exactly the same time.
///
/// `#[track_caller]` records the acquisition site without changing hundreds of
/// call sites. The guard reports both excessive acquisition waits and holds;
/// the latter identifies the actual holder that stalled the rest of the app.
pub(crate) const STATE_MUTEX_WARN_AFTER: Duration = Duration::from_millis(250);

pub(crate) const STATE_MUTEX_DIAGNOSTIC_QUEUE_CAPACITY: usize = 64;

/// Aggregate timings only: no workspace paths or transcript content. Path
/// checks are a subset of the inclusive scan, not another additive stage.
/// path_check_* measures the held-lock decision on a frozen match result;
/// off-lock canonicalization and containment I/O belong to preparation.
#[derive(Clone, Debug, Default)]
pub(crate) struct FileChangeTrackingDiagnostic {
    pub(crate) preparation: Duration,
    pub(crate) waited: Duration,
    pub(crate) held_work: Duration,
    pub(crate) scan_inclusive: Duration,
    pub(crate) path_checks_subset: Duration,
    pub(crate) path_check_max: Duration,
    pub(crate) late_summaries: Duration,
    pub(crate) commit: Duration,
    pub(crate) remainder: Duration,
    pub(crate) input_changes: usize,
    pub(crate) visited_sessions: usize,
    pub(crate) eligible_sessions: usize,
    pub(crate) path_checks: usize,
    pub(crate) path_matches: usize,
    pub(crate) late_summary_count: usize,
    pub(crate) commit_attempted: bool,
    pub(crate) commit_failed: bool,
}

impl FileChangeTrackingDiagnostic {
    pub(crate) fn elapsed(&self) -> Duration {
        self.preparation + self.waited + self.held_work
    }

    pub(crate) fn finish_held_work(&mut self, held_work: Duration) {
        self.held_work = held_work;
        // Do not subtract path_checks_subset again: it is already in scan.
        self.remainder =
            held_work.saturating_sub(self.scan_inclusive + self.late_summaries + self.commit);
    }
}

impl std::fmt::Display for FileChangeTrackingDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let milliseconds = |duration: Duration| duration.as_secs_f64() * 1_000.0;
        write!(
            formatter,
            concat!(
                "preparation_ms={:.3} waited_ms={:.3} held_work_ms={:.3} ",
                "scan_inclusive_ms={:.3} path_checks_subset_ms={:.3} path_check_max_ms={:.3} ",
                "late_summaries_ms={:.3} commit_ms={:.3} remainder_ms={:.3} ",
                "input_changes={} visited_sessions={} eligible_sessions={} path_checks={} ",
                "path_matches={} late_summary_count={} commit_attempted={} commit_failed={}"
            ),
            milliseconds(self.preparation),
            milliseconds(self.waited),
            milliseconds(self.held_work),
            milliseconds(self.scan_inclusive),
            milliseconds(self.path_checks_subset),
            milliseconds(self.path_check_max),
            milliseconds(self.late_summaries),
            milliseconds(self.commit),
            milliseconds(self.remainder),
            self.input_changes,
            self.visited_sessions,
            self.eligible_sessions,
            self.path_checks,
            self.path_matches,
            self.late_summary_count,
            self.commit_attempted,
            self.commit_failed,
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) enum StateMutexDiagnostic {
    FileChangeTracking(FileChangeTrackingDiagnostic),
    Waited {
        waited: Duration,
        file: &'static str,
        line: u32,
        column: u32,
    },
    Held {
        held: Duration,
        waited: Duration,
        file: &'static str,
        line: u32,
        column: u32,
    },
}

pub(crate) type StateMutexDiagnosticReporter = Arc<dyn Fn(StateMutexDiagnostic) + Send + Sync>;

#[cfg(not(test))]
fn state_mutex_diagnostic_sender() -> &'static mpsc::SyncSender<StateMutexDiagnostic> {
    static SENDER: LazyLock<mpsc::SyncSender<StateMutexDiagnostic>> = LazyLock::new(|| {
        let (sender, receiver) = mpsc::sync_channel(STATE_MUTEX_DIAGNOSTIC_QUEUE_CAPACITY);
        if let Err(err) = std::thread::Builder::new()
            .name("termal-state-lock-diagnostics".to_owned())
            .spawn(move || {
                while let Ok(diagnostic) = receiver.recv() {
                    match diagnostic {
                        StateMutexDiagnostic::FileChangeTracking(timings) => eprintln!(
                            "state lock> file-change tracking {timings}"
                        ),
                        StateMutexDiagnostic::Waited {
                            waited,
                            file,
                            line,
                            column,
                        } => eprintln!(
                            "state lock> waited {:.0}ms at {file}:{line}:{column}",
                            waited.as_secs_f64() * 1_000.0,
                        ),
                        StateMutexDiagnostic::Held {
                            held,
                            waited,
                            file,
                            line,
                            column,
                        } => eprintln!(
                            "state lock> held {:.0}ms by {file}:{line}:{column} (acquisition wait {:.0}ms)",
                            held.as_secs_f64() * 1_000.0,
                            waited.as_secs_f64() * 1_000.0,
                        ),
                    }
                }
            })
        {
            eprintln!("state lock> failed to start diagnostic reporter: {err}");
        }
        sender
    });
    &SENDER
}

#[cfg(not(test))]
fn report_state_mutex_diagnostic(diagnostic: StateMutexDiagnostic) {
    // Diagnostics must never extend a state-lock convoy. A saturated or
    // unavailable reporter deliberately drops the warning instead of waiting.
    let _ = try_queue_state_mutex_diagnostic(state_mutex_diagnostic_sender(), diagnostic);
}

pub(crate) fn try_queue_state_mutex_diagnostic(
    sender: &mpsc::SyncSender<StateMutexDiagnostic>,
    diagnostic: StateMutexDiagnostic,
) -> bool {
    sender.try_send(diagnostic).is_ok()
}

fn default_state_mutex_diagnostic_reporter() -> StateMutexDiagnosticReporter {
    #[cfg(not(test))]
    {
        let _ = state_mutex_diagnostic_sender();
        return Arc::new(report_state_mutex_diagnostic);
    }

    #[cfg(test)]
    Arc::new(|_| {})
}

/// The longest hold at or above the warning threshold since it was last taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StateMutexLongHold {
    pub(crate) held: Duration,
    pub(crate) location: &'static std::panic::Location<'static>,
}

#[cfg(test)]
thread_local! {
    // One-shot schedule witness only; it neither supplies a guard nor records
    // a diagnostic. The real mutex still decides acquisition and poisoning.
    pub(crate) static TEST_STATE_MUTEX_REQUESTED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    pub(crate) static TEST_STATE_MUTEX_UNLOCKED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

pub(crate) struct StateMutex<T> {
    pub(crate) inner: Mutex<T>,
    pub(crate) lock_observer: Arc<StateLockObserver>,
    pub(crate) diagnostic_reporter: StateMutexDiagnosticReporter,
    pub(crate) warn_after: Duration,
    // Set only on the existing over-threshold report path, never per lock.
    longest_hold: Mutex<Option<StateMutexLongHold>>,
    /// Test-only owner tracking distinguishes a callback that re-enters this
    /// exact mutex on the same thread from harmless contention by a fixture's
    /// background thread. That makes lock-scope assertions deterministic.
    #[cfg(test)]
    owner_thread: Mutex<Option<std::thread::ThreadId>>,
}

impl<T: 'static> StateMutex<T> {
    /// Call only after releasing the guard. The production reporter queues
    /// without waiting and formats on its own thread, including on slow scans
    /// which did not produce a late summary or commit.
    pub(crate) fn report_file_change_tracking(&self, diagnostic: FileChangeTrackingDiagnostic) {
        if diagnostic.elapsed() >= self.warn_after {
            (self.diagnostic_reporter)(StateMutexDiagnostic::FileChangeTracking(diagnostic));
        }
    }

    #[cfg(test)]
    pub(crate) fn new(value: T) -> Self {
        Self::new_for_instance(value, &STATE_LOCK_PROCESS)
    }

    pub(crate) fn new_for_instance(value: T, instance: &str) -> Self {
        Self {
            inner: Mutex::new(value),
            lock_observer: Arc::new(StateLockObserver::new(instance)),
            diagnostic_reporter: default_state_mutex_diagnostic_reporter(),
            warn_after: STATE_MUTEX_WARN_AFTER,
            longest_hold: Mutex::new(None),
            #[cfg(test)]
            owner_thread: Mutex::new(None),
        }
    }

    /// The longest over-threshold hold since the previous call, then reset.
    pub(crate) fn take_longest_hold(&self) -> Option<StateMutexLongHold> {
        self.longest_hold
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    #[cfg(test)]
    pub(crate) fn new_with_diagnostic_reporter(
        value: T,
        warn_after: Duration,
        diagnostic_reporter: StateMutexDiagnosticReporter,
    ) -> Self {
        Self {
            inner: Mutex::new(value),
            lock_observer: Arc::new(StateLockObserver::new(&STATE_LOCK_PROCESS)),
            diagnostic_reporter,
            warn_after,
            longest_hold: Mutex::new(None),
            owner_thread: Mutex::new(None),
        }
    }

    #[track_caller]
    pub(crate) fn lock(&self) -> std::sync::LockResult<StateMutexGuard<'_, T>> {
        let requested_at = std::time::Instant::now();
        let caller = std::panic::Location::caller();
        #[cfg(test)]
        TEST_STATE_MUTEX_REQUESTED.with(|slot| {
            let hook = slot.borrow_mut().take();
            if let Some(hook) = hook {
                hook();
            }
        });
        match self.inner.lock() {
            Ok(guard) => Ok(StateMutexGuard::new(
                guard,
                requested_at,
                caller,
                self.warn_after,
                Arc::clone(&self.diagnostic_reporter),
                &self.longest_hold,
                Arc::clone(&self.lock_observer),
                #[cfg(test)]
                &self.owner_thread,
            )),
            Err(err) => Err(std::sync::PoisonError::new(StateMutexGuard::new(
                err.into_inner(),
                requested_at,
                caller,
                self.warn_after,
                Arc::clone(&self.diagnostic_reporter),
                &self.longest_hold,
                Arc::clone(&self.lock_observer),
                #[cfg(test)]
                &self.owner_thread,
            ))),
        }
    }

    /// `lock` without waiting: `None` when another thread holds the state,
    /// for a caller that must not extend its own deadline to look.
    #[track_caller]
    pub(crate) fn try_lock_now(&self) -> Option<StateMutexGuard<'_, T>> {
        let requested_at = std::time::Instant::now();
        let caller = std::panic::Location::caller();
        let guard = match self.inner.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        Some(StateMutexGuard::new(
            guard,
            requested_at,
            caller,
            self.warn_after,
            Arc::clone(&self.diagnostic_reporter),
            &self.longest_hold,
            Arc::clone(&self.lock_observer),
            #[cfg(test)]
            &self.owner_thread,
        ))
    }

    #[cfg(test)]
    pub(crate) fn is_not_held_by_current_thread_for_test(&self) -> bool {
        let owner = self
            .owner_thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match owner.as_ref() {
            Some(owner) => owner != &std::thread::current().id(),
            None => true,
        }
    }
}

pub(crate) struct StateMutexGuard<'a, T> {
    pub(crate) guard: Option<std::sync::MutexGuard<'a, T>>,
    pub(crate) acquired_at: std::time::Instant,
    pub(crate) caller: &'static std::panic::Location<'static>,
    pub(crate) diagnostic_reporter: StateMutexDiagnosticReporter,
    pub(crate) lock_observer: Arc<StateLockObserver>,
    pub(crate) lock_span: StateLockSpan,
    pub(crate) waited: Duration,
    pub(crate) warn_after: Duration,
    longest_hold: &'a Mutex<Option<StateMutexLongHold>>,
    #[cfg(test)]
    owner_thread: &'a Mutex<Option<std::thread::ThreadId>>,
}

impl<'a, T: 'static> StateMutexGuard<'a, T> {
    pub(crate) fn new(
        guard: std::sync::MutexGuard<'a, T>,
        requested_at: std::time::Instant,
        caller: &'static std::panic::Location<'static>,
        warn_after: Duration,
        diagnostic_reporter: StateMutexDiagnosticReporter,
        longest_hold: &'a Mutex<Option<StateMutexLongHold>>,
        lock_observer: Arc<StateLockObserver>,
        #[cfg(test)] owner_thread: &'a Mutex<Option<std::thread::ThreadId>>,
    ) -> Self {
        let acquired_at = std::time::Instant::now();
        let epoch = state_lock_value_epoch(&*guard, lock_observer.fallback_epoch);
        let lock_span = lock_observer.acquired(epoch, caller, requested_at, acquired_at);
        let waited = acquired_at.saturating_duration_since(requested_at);
        if waited >= warn_after {
            diagnostic_reporter(StateMutexDiagnostic::Waited {
                waited,
                file: caller.file(),
                line: caller.line(),
                column: caller.column(),
            });
        }
        #[cfg(test)]
        {
            let mut owner = owner_thread
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            debug_assert!(owner.is_none());
            *owner = Some(std::thread::current().id());
        }
        Self {
            guard: Some(guard),
            acquired_at,
            caller,
            diagnostic_reporter,
            lock_observer,
            lock_span,
            waited,
            warn_after,
            longest_hold,
            #[cfg(test)]
            owner_thread,
        }
    }
}

impl<T> std::ops::Deref for StateMutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.guard
            .as_deref()
            .expect("state mutex guard accessed after release")
    }
}

impl<T> std::ops::DerefMut for StateMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard
            .as_deref_mut()
            .expect("state mutex guard accessed after release")
    }
}

impl<T> Drop for StateMutexGuard<'_, T> {
    fn drop(&mut self) {
        let mut span = self.lock_span;
        span.released = Some(state_lock_point(std::time::Instant::now()));
        let held = self.acquired_at.elapsed();
        let waited = self.waited;
        let file = self.caller.file();
        let line = self.caller.line();
        let column = self.caller.column();
        #[cfg(test)]
        {
            let mut owner = self
                .owner_thread
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            debug_assert_eq!(owner.as_ref(), Some(&std::thread::current().id()));
            *owner = None;
        }
        // Publish the release in flight before handing the primary mutex to
        // a successor. History remains an off-lock, nonblocking append.
        self.lock_observer.begin_release();
        drop(self.guard.take());
        #[cfg(test)]
        TEST_STATE_MUTEX_UNLOCKED.with(|slot| {
            let hook = slot.borrow_mut().take();
            if let Some(hook) = hook {
                hook();
            }
        });
        self.lock_observer.finish_release(span);
        if held >= self.warn_after {
            // After release, on the rare over-threshold path only.
            let mut longest = self
                .longest_hold
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if longest.is_none_or(|longest| held > longest.held) {
                *longest = Some(StateMutexLongHold {
                    held,
                    location: self.caller,
                });
            }
            drop(longest);
            (self.diagnostic_reporter)(StateMutexDiagnostic::Held {
                held,
                waited,
                file,
                line,
                column,
            });
        }
    }
}

pub(crate) enum StateBroadcastWork {
    Snapshot(StateResponse),
    ImmutableSnapshot(ImmutableStateSnapshot),
    Drain,
    Delta(DeltaEvent),
    /// Pending work was dropped at capacity: consumers must resync. A loss
    /// signal outside the commit order, with no revision of its own.
    Lagged,
}

#[cfg(test)]
impl StateBroadcastWork {
    pub(crate) fn revision(&self) -> Option<u64> {
        match self {
            Self::Snapshot(snapshot) => Some(snapshot.revision),
            Self::ImmutableSnapshot(snapshot) => Some(snapshot.header.revision),
            Self::Drain => None,
            Self::Delta(event) => Some(event.revision()),
            Self::Lagged => None,
        }
    }
}

pub(crate) const STATE_BROADCAST_MAILBOX_CAPACITY: usize = 256;

#[derive(Default)]
pub(crate) struct StateBroadcastPending {
    pub(crate) work: VecDeque<StateBroadcastWork>,
    pub(crate) lagged: bool,
    pub(crate) closing: bool,
    pub(crate) stopped: bool,
}

impl StateBroadcastPending {
    pub(crate) fn enqueue(&mut self, work: StateBroadcastWork) {
        if self.work.len() >= STATE_BROADCAST_MAILBOX_CAPACITY {
            self.work.pop_front();
            // Kept outside the bounded queue: further overflow cannot evict
            // the recovery signal. The consumer sees it before retained work.
            self.lagged = true;
        }
        self.work.push_back(work);
    }

    pub(crate) fn pop_front(&mut self) -> Option<StateBroadcastWork> {
        if std::mem::take(&mut self.lagged) {
            Some(StateBroadcastWork::Lagged)
        } else {
            self.work.pop_front()
        }
    }
}

#[derive(Default)]
pub(crate) struct StateBroadcastMailbox {
    pub(crate) pending: Mutex<StateBroadcastPending>,
    pub(crate) work_available: Condvar,
    #[cfg(test)]
    pub(crate) before_forward: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl StateBroadcastMailbox {
    pub(crate) fn publish_snapshot(&self, snapshot: StateResponse) {
        self.publish_snapshot_work(StateBroadcastWork::Snapshot(snapshot));
    }

    pub(crate) fn publish_snapshot_work(&self, work: StateBroadcastWork) {
        let mut pending = self
            .pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned");
        debug_assert!(!pending.stopped);
        if matches!(
            pending.work.back(),
            Some(StateBroadcastWork::Snapshot(_) | StateBroadcastWork::ImmutableSnapshot(_))
        ) {
            *pending.work.back_mut().unwrap() = work;
        } else {
            pending.enqueue(work);
        }
        self.work_available.notify_one();
    }

    pub(crate) fn publish_delta(&self, event: DeltaEvent) {
        let mut pending = self
            .pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned");
        pending.enqueue(StateBroadcastWork::Delta(event));
        self.work_available.notify_one();
    }

    pub(crate) fn recv_next(&self) -> StateBroadcastWork {
        let mut pending = self
            .pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned");
        loop {
            if let Some(work) = pending.pop_front() {
                return work;
            }
            if pending.closing {
                return StateBroadcastWork::Drain;
            }
            pending = self
                .work_available
                .wait(pending)
                .expect("state broadcast mailbox mutex poisoned");
        }
    }

    #[cfg(test)]
    pub(crate) fn take_pending_for_test(&self) -> Vec<StateBroadcastWork> {
        let mut pending = self
            .pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned");
        let mut drained = Vec::new();
        while let Some(work) = pending.pop_front() {
            drained.push(work);
        }
        drained
    }
}

/// One `/api/events` event, a state snapshot, a state diff or a delta, as the
/// broadcaster published it (`StateBroadcastSenders::stream_events`).
#[derive(Clone, Debug)]
pub(crate) enum SseStreamEvent {
    /// A full snapshot every connection forwards. The broadcaster marks one it
    /// sends as a diff base with `stateDiffBase: true`; snapshots from paths
    /// that never diff carry no mark.
    State(Arc<str>),
    /// A full snapshot of the same cut as an accompanying `StateDiff`, sent
    /// only while legacy connections exist; opted-in connections drop it.
    LegacyState(Arc<str>),
    /// A cumulative diff from the broadcaster's last emitted cut, forwarded
    /// only by connections that opted in (`/api/events?stateDiff=1`).
    StateDiff(Arc<str>),
    Delta(Arc<str>),
    Lagged,
}

/// The broadcast channels a published snapshot or delta goes to.
///
/// `stream_events` carries snapshots and deltas together, in the order they
/// were published: it is what `/api/events` streams. Two channels merged by
/// the reader cannot keep that order, and a snapshot that overtakes an older
/// delta makes the client drop the delta while the snapshot carries none of
/// its message. Test builds also send each payload to a per-kind channel, so a
/// test can observe snapshots or deltas alone; production builds have no such
/// channel and make no second copy of a payload.
///
/// `legacy_state_subscribers` counts live `/api/events` connections that did
/// not opt into `stateDiff`. The broadcaster serializes a full snapshot beside
/// each diff only while that count is positive.
/// `full_state_request_generation` counts requests for a marked full state (a
/// new diff base): an opted-in connection and `POST /api/events/full-state`
/// each increment it. Every cut records the generation it was captured at, so
/// only a cut captured after a request can answer it.
#[derive(Clone)]
pub(crate) struct StateBroadcastSenders {
    pub(crate) stream_events: broadcast::Sender<SseStreamEvent>,
    pub(crate) legacy_state_subscribers: Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) full_state_request_generation: Arc<AtomicU64>,
    pub(crate) diff_counters: Arc<StateDiffCounters>,
    #[cfg(test)]
    pub(crate) state_events: broadcast::Sender<String>,
    #[cfg(test)]
    pub(crate) delta_events: broadcast::Sender<String>,
    #[cfg(test)]
    pub(crate) state_diff_events: broadcast::Sender<String>,
}

impl StateBroadcastSenders {
    /// Sends one serialized state snapshot that every connection forwards.
    /// The ordered stream gets it first, so a test that has seen a payload on
    /// a per-kind channel knows the ordered stream already holds it. Returns
    /// whether the ordered stream had a receiver.
    pub(crate) fn send_state(&self, payload: String) -> bool {
        #[cfg(test)]
        let observed = payload.clone();
        let sent = self
            .stream_events
            .send(SseStreamEvent::State(Arc::from(payload)))
            .is_ok();
        #[cfg(test)]
        let _ = self.state_events.send(observed);
        sent
    }

    /// Sends the full snapshot that accompanies a diff of the same cut, for
    /// legacy connections only. Test builds observe it with other snapshots.
    pub(crate) fn send_legacy_state(&self, payload: String) -> bool {
        #[cfg(test)]
        let observed = payload.clone();
        let sent = self
            .stream_events
            .send(SseStreamEvent::LegacyState(Arc::from(payload)))
            .is_ok();
        #[cfg(test)]
        let _ = self.state_events.send(observed);
        sent
    }

    /// Sends one serialized state diff, in the same order as the others.
    pub(crate) fn send_state_diff(&self, payload: String) -> bool {
        #[cfg(test)]
        let observed = payload.clone();
        let sent = self
            .stream_events
            .send(SseStreamEvent::StateDiff(Arc::from(payload)))
            .is_ok();
        #[cfg(test)]
        let _ = self.state_diff_events.send(observed);
        sent
    }

    /// Whether a full snapshot must accompany a diff. Test builds also count
    /// observers of the per-kind snapshot channel, which expect every cut as
    /// a full snapshot exactly as a legacy connection does.
    pub(crate) fn legacy_state_demand(&self) -> bool {
        let legacy = self
            .legacy_state_subscribers
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0;
        #[cfg(test)]
        let legacy = legacy || self.state_events.receiver_count() > 0;
        legacy
    }

    /// Sends one serialized delta, in the same order as [`Self::send_state`].
    #[cfg(test)]
    pub(crate) fn send_delta(&self, payload: String) {
        let observed = payload.clone();
        let _ = self
            .stream_events
            .send(SseStreamEvent::Delta(Arc::from(payload)));
        let _ = self.delta_events.send(observed);
    }

    pub(crate) fn send_delta_event(&self, event: &DeltaEvent) -> bool {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ClassifiedDelta<'a> {
            #[serde(flatten)]
            event: &'a DeltaEvent,
            mirrored_delegation_child_ids: Vec<&'a str>,
        }
        let ids = mirrored_delegation_child_ids(event.session_summaries()).collect::<Vec<_>>();
        let encoded = if ids.is_empty() {
            encode_sse_json("delta", event)
        } else {
            encode_sse_json(
                "delta",
                &ClassifiedDelta {
                    event,
                    mirrored_delegation_child_ids: ids,
                },
            )
        };
        let payload = match encoded {
            Ok(payload) => payload,
            Err(error) => {
                observe_output_limit("sse_delta", &error, Some(event.revision()));
                let _ = self.stream_events.send(SseStreamEvent::Lagged);
                return false;
            }
        };
        // The private lifecycle channel keeps the original canonical event
        // for custody tests. Public transport receives counters/invalidation
        // only; its integration tests observe stream_events.
        #[cfg(test)]
        let observed = match event {
            DeltaEvent::TestRunsInvalidated {
                internal: Some(event),
                ..
            } => Some(serde_json::to_string(event).expect("canonical test-run event")),
            DeltaEvent::TestRunsInvalidated { internal: None, .. } => None,
            _ => Some(payload.clone()),
        };
        let _ = self
            .stream_events
            .send(SseStreamEvent::Delta(Arc::from(payload)));
        #[cfg(test)]
        if let Some(observed) = observed {
            let _ = self.delta_events.send(observed);
        }
        true
    }
}

pub(crate) fn forward_state_broadcast_work(
    work: StateBroadcastWork,
    senders: &StateBroadcastSenders,
) -> bool {
    #[cfg(test)]
    TEST_BEFORE_STATE_SERIALIZE.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
    match work {
        StateBroadcastWork::Snapshot(snapshot) => match encode_sse_json("state", &snapshot) {
            Ok(payload) => {
                senders.send_state(payload);
                true
            }
            Err(err) => {
                observe_output_limit("sse_state", &err, Some(snapshot.revision));
                let _ = senders.stream_events.send(SseStreamEvent::Lagged);
                false
            }
        },
        StateBroadcastWork::ImmutableSnapshot(snapshot) => forward_state_broadcast_work(
            StateBroadcastWork::Snapshot(snapshot.materialize()),
            senders,
        ),
        StateBroadcastWork::Drain => panic!("drain is handled by the owned broadcaster"),
        StateBroadcastWork::Delta(event) => senders.send_delta_event(&event),
        StateBroadcastWork::Lagged => {
            let _ = senders.stream_events.send(SseStreamEvent::Lagged);
            true
        }
    }
}

#[cfg(test)]
mod state_broadcast_mailbox_tests {
    use super::*;

    #[test]
    fn state_broadcast_mailbox_overflow_preserves_a_recovery_notification() {
        let mailbox = StateBroadcastMailbox::default();
        for revision in 0..=STATE_BROADCAST_MAILBOX_CAPACITY as u64 {
            mailbox.publish_delta(delta(revision));
        }
        let pending = mailbox.take_pending_for_test();
        assert_eq!(
            pending.len(),
            STATE_BROADCAST_MAILBOX_CAPACITY + 1,
            "retained work must include a recovery marker for the lost delta"
        );
        assert!(matches!(pending.first(), Some(StateBroadcastWork::Lagged)));
        // The oldest delta was dropped; the retained ones keep their order.
        assert_eq!(pending[1].revision(), Some(1));
        assert_eq!(
            pending.last().and_then(StateBroadcastWork::revision),
            Some(STATE_BROADCAST_MAILBOX_CAPACITY as u64)
        );
        assert!(mailbox.take_pending_for_test().is_empty());
    }

    #[test]
    fn state_broadcast_mailbox_keeps_the_loss_signal_through_coalescing_and_signals_again() {
        let mailbox = StateBroadcastMailbox::default();
        for revision in 0..=STATE_BROADCAST_MAILBOX_CAPACITY as u64 {
            mailbox.publish_delta(delta(revision));
        }
        // Replacing an adjacent snapshot neither clears the flag nor counts
        // as a new overflow.
        mailbox.publish_snapshot(snapshot(1_000));
        mailbox.publish_snapshot(snapshot(1_001));
        let pending = mailbox.take_pending_for_test();
        assert!(matches!(pending.first(), Some(StateBroadcastWork::Lagged)));
        assert_eq!(
            pending
                .iter()
                .filter(|work| matches!(work, StateBroadcastWork::Lagged))
                .count(),
            1
        );
        assert!(
            matches!(pending.last(), Some(StateBroadcastWork::Snapshot(value)) if value.revision == 1_001)
        );

        // Once taken, a later overflow signals again.
        for revision in 0..STATE_BROADCAST_MAILBOX_CAPACITY as u64 {
            mailbox.publish_delta(delta(revision));
        }
        assert!(
            !mailbox
                .take_pending_for_test()
                .iter()
                .any(|work| matches!(work, StateBroadcastWork::Lagged)),
            "a full queue without a loss carries no signal"
        );
        for revision in 0..=STATE_BROADCAST_MAILBOX_CAPACITY as u64 {
            mailbox.publish_delta(delta(revision));
        }
        assert!(matches!(
            mailbox.take_pending_for_test().first(),
            Some(StateBroadcastWork::Lagged)
        ));
    }

    fn snapshot(revision: u64) -> StateResponse {
        StateResponse {
            remote_transport_statuses: Vec::new(),
            revision,
            server_instance_id: "test-instance".to_owned(),
            codex: CodexState::default(),
            agent_readiness: Vec::new(),
            preferences: AppPreferences::default(),
            projects: Vec::new(),
            orchestrators: Vec::new(),
            workspaces: Vec::new(),
            sessions: Vec::new(),
            mirrored_delegation_child_ids: Vec::new(),
            queue_pause_reasons: BTreeMap::new(),
            delegations: Vec::new(),
            delegation_waits: Vec::new(),
            test_run_overview: TestRunOverviewSnapshot::default(),
            test_run_waits: Vec::new(),
            pending_engram_mcp_revocation_session_ids: Vec::new(),
        }
    }

    fn delta(revision: u64) -> DeltaEvent {
        DeltaEvent::TestRunRemoved {
            revision,
            run_id: format!("run-{revision}"),
        }
    }

    #[test]
    fn state_broadcast_mailbox_keeps_only_latest_pending_snapshot() {
        let mailbox = StateBroadcastMailbox::default();
        for revision in 1..=3 {
            mailbox.publish_snapshot(snapshot(revision));
        }
        let pending = mailbox.take_pending_for_test();
        assert_eq!(pending.len(), 1);
        assert!(matches!(&pending[0], StateBroadcastWork::Snapshot(value) if value.revision == 3));
        assert!(mailbox.take_pending_for_test().is_empty());
    }

    #[test]
    fn state_broadcast_mailbox_preserves_state_before_following_delta() {
        let mailbox = StateBroadcastMailbox::default();
        mailbox.publish_snapshot(snapshot(1));
        mailbox.publish_snapshot(snapshot(2));
        mailbox.publish_delta(delta(3));
        mailbox.publish_snapshot(snapshot(4));
        mailbox.publish_snapshot(snapshot(5));
        let pending = mailbox.take_pending_for_test();
        assert_eq!(
            pending
                .iter()
                .map(StateBroadcastWork::revision)
                .collect::<Vec<_>>(),
            [Some(2), Some(3), Some(5)]
        );
        assert!(
            matches!(&pending[1], StateBroadcastWork::Delta(DeltaEvent::TestRunRemoved { run_id, .. }) if run_id == "run-3")
        );
    }

    #[test]
    fn state_broadcast_mailbox_coalesces_latest_snapshot_when_full() {
        let mailbox = StateBroadcastMailbox::default();
        for revision in 1..STATE_BROADCAST_MAILBOX_CAPACITY as u64 {
            mailbox.publish_delta(delta(revision));
        }
        mailbox.publish_snapshot(snapshot(256));
        mailbox.publish_snapshot(snapshot(257));
        let pending = mailbox.take_pending_for_test();
        assert_eq!(pending.len(), STATE_BROADCAST_MAILBOX_CAPACITY);
        assert!(
            matches!(pending.last(), Some(StateBroadcastWork::Snapshot(value)) if value.revision == 257)
        );
    }
}

pub(crate) const REMOTE_DELTA_REPLAY_CACHE_LIMIT: usize = 2048;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RemoteDeltaReplayKey {
    pub(crate) remote_id: String,
    /// Identifies the connection/config publication that owned this event.
    /// The same remote id, revision, and payload from a replacement endpoint
    /// must never be suppressed by bookkeeping left by its predecessor.
    pub(crate) authority_generation: u64,
    pub(crate) revision: u64,
    pub(crate) payload: RemoteDeltaReplayPayload,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RemoteDeltaHydrationKey {
    pub(crate) remote_id: String,
    pub(crate) remote_session_id: String,
    pub(crate) authority_generation: u64,
}

/// Semantic identity for one remote delta replay key.
///
/// Every state-mutating field from the corresponding `DeltaEvent` variant must
/// be represented here directly or through a stable fingerprint. The replay
/// cache uses this value to distinguish exact same-revision redeliveries from
/// valid same-revision sibling deltas. See `wire::DeltaEvent` for the source
/// variants and `AppState::apply_remote_delta_event` for the consumer; new wire
/// fields must be added here and pinned by the `remote_delta_replay_key_*`
/// tests.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RemoteDeltaReplayPayload {
    SessionCreated {
        session_id: String,
        message_count: u32,
        session_projection: (String, bool),
        session_mutation_stamp: Option<u64>,
    },
    MessageCreated {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        message_fingerprint: String,
        preview_fingerprint: String,
        status: u8,
        session_queue_fingerprint: Option<String>,
        session_mutation_stamp: Option<u64>,
        session_seq: Option<u64>,
        body_seq_epoch: Option<String>,
    },
    MessageUpdated {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        message_fingerprint: String,
        preview_fingerprint: String,
        status: u8,
        session_mutation_stamp: Option<u64>,
        session_seq: Option<u64>,
        body_seq_epoch: Option<String>,
    },
    TextDelta {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        text_start_byte: usize,
        delta_fingerprint: String,
        preview_fingerprint: Option<String>,
        session_mutation_stamp: Option<u64>,
        session_seq: Option<u64>,
        body_seq_epoch: Option<String>,
    },
    TextReplace {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        text_fingerprint: String,
        preview_fingerprint: Option<String>,
        session_mutation_stamp: Option<u64>,
        session_seq: Option<u64>,
        body_seq_epoch: Option<String>,
    },
    CommandUpdate {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        command_fingerprint: String,
        command_language: Option<String>,
        output_fingerprint: String,
        output_language: Option<String>,
        status: u8,
        preview_fingerprint: String,
        session_mutation_stamp: Option<u64>,
        session_seq: Option<u64>,
        body_seq_epoch: Option<String>,
    },
    ParallelAgentsUpdate {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        agents_fingerprint: String,
        preview_fingerprint: String,
        session_mutation_stamp: Option<u64>,
        session_seq: Option<u64>,
        body_seq_epoch: Option<String>,
    },
    TestRunCardUpdated {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        run_fingerprint: String,
        preview_fingerprint: String,
        session_mutation_stamp: Option<u64>,
        session_seq: Option<u64>,
        body_seq_epoch: Option<String>,
    },
    ConversationMarkerCreated {
        session_id: String,
        marker_id: String,
        marker_fingerprint: String,
        session_mutation_stamp: Option<u64>,
    },
    ConversationMarkerUpdated {
        session_id: String,
        marker_id: String,
        marker_fingerprint: String,
        session_mutation_stamp: Option<u64>,
    },
    ConversationMarkerDeleted {
        session_id: String,
        marker_id: String,
        session_mutation_stamp: Option<u64>,
    },
    CodexUpdated {
        codex_fingerprint: String,
    },
    OrchestratorsUpdated {
        orchestrator_fingerprints: Vec<String>,
        session_projections: Vec<(String, bool)>,
    },
}

/// Bounded exact remote-delta replay suppression.
///
/// Entries are keyed by remote id, remote revision, and semantic delta payload
/// identity. The per-remote cap is intentionally small because the cache only
/// covers the short same-revision replay window around remote event delivery;
/// older revisions still fall back to the monotonic remote-applied watermark.
/// Per-remote entries are cleared when event-stream continuity is lost. Memory
/// grows linearly with active remote count (`remotes * limit`), which is the
/// fairness tradeoff versus the previous globally-FIFO cache. Insertion is
/// O(total cache size) once a remote is over cap because we scan the shared
/// order queue to evict that remote's oldest entry; this is acceptable while
/// the cache is bounded and only records successfully applied deltas. If this
/// appears in profiles, split storage into per-remote queues to restore O(1)
/// eviction without losing cross-remote isolation.
#[derive(Default)]
pub(crate) struct RemoteDeltaReplayCache {
    pub(crate) keys: HashSet<RemoteDeltaReplayKey>,
    pub(crate) order: VecDeque<RemoteDeltaReplayKey>,
}

impl RemoteDeltaReplayCache {
    pub(crate) fn contains(&self, key: &RemoteDeltaReplayKey) -> bool {
        self.keys.contains(key)
    }

    pub(crate) fn insert(&mut self, key: RemoteDeltaReplayKey) {
        if self.keys.contains(&key) {
            return;
        }
        let remote_id = key.remote_id.clone();
        self.order.push_back(key.clone());
        self.keys.insert(key);
        let mut remote_entry_count = self
            .order
            .iter()
            .filter(|entry| entry.remote_id == remote_id)
            .count();
        while remote_entry_count > REMOTE_DELTA_REPLAY_CACHE_LIMIT {
            let Some(expired_index) = self
                .order
                .iter()
                .position(|entry| entry.remote_id == remote_id)
            else {
                break;
            };
            if let Some(expired) = self.order.remove(expired_index) {
                self.keys.remove(&expired);
                remote_entry_count -= 1;
            }
        }
    }

    pub(crate) fn remove_remote(&mut self, remote_id: &str) -> bool {
        let previous_len = self.keys.len();
        self.keys.retain(|key| key.remote_id != remote_id);
        self.order.retain(|key| key.remote_id != remote_id);
        self.keys.len() != previous_len
    }
}

/// Lightweight selection captured in one pass under `AppState::inner`.
///
/// Session transcripts and delegation payloads are deliberately represented by
/// ids here. The persistence worker materializes those large values one record
/// at a time, releasing the global state mutex between records so a batch of
/// dirty transcripts cannot form one long application-wide lock convoy.
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct PersistDeltaPlan {
    pub(crate) metadata: PersistedState,
    pub(crate) changed_sessions: Vec<PersistSessionCandidate>,
    pub(crate) removed_session_ids: Vec<String>,
    pub(crate) changed_delegations: Vec<PersistDelegationCandidate>,
    pub(crate) removed_delegation_ids: Vec<String>,
    pub(crate) drained_delegation_tombstones: BTreeMap<String, u64>,
    pub(crate) drained_explicit_tombstones: Vec<String>,
    pub(crate) watermark: u64,
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) struct PersistSessionCandidate {
    pub(crate) session_id: String,
    pub(crate) mutation_stamp: u64,
    pub(crate) persist_prompt_history: bool,
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) struct PersistDelegationCandidate {
    pub(crate) delegation_id: String,
    pub(crate) mutation_stamp: u64,
}

pub(crate) enum PersistCandidateMaterialization<T> {
    Snapshot(T),
    Changed,
}

/// The fully materialized diff the persist thread writes on each tick.
///
/// `changed_sessions` contains only sessions selected by a preceding
/// [`PersistDeltaPlan`]. `removed_session_ids` is the union of explicit
/// removals and sessions that were hidden when selected. Candidates that
/// change before materialization are omitted from that pass; the plan watermark
/// advances, and their later mutation stamp or tombstone selects them again.
/// Production performs one bounded re-plan pass before returning the delta and
/// retains the remaining deferred ids for diagnostics.
/// The persist thread writes the delta to SQLite with targeted
/// session/delegation upserts and deletes.
/// `drained_explicit_tombstones` keeps only tombstones drained from state so a
/// failed write can restore those without duplicating synthesized deletes.
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct PersistDelta {
    pub(crate) metadata: PersistedState,
    pub(crate) changed_sessions: Vec<PersistedSessionRecord>,
    pub(crate) removed_session_ids: Vec<String>,
    pub(crate) changed_delegations: Option<Vec<DelegationRecord>>,
    pub(crate) removed_delegation_ids: Vec<String>,
    pub(crate) deferred_session_ids: Vec<String>,
    pub(crate) deferred_prompt_history_session_ids: Vec<String>,
    pub(crate) deferred_delegation_ids: Vec<String>,
    pub(crate) drained_delegation_tombstones: BTreeMap<String, u64>,
    pub(crate) drained_explicit_tombstones: Vec<String>,
    pub(crate) watermark: u64,
}

#[derive(Clone)]
pub(crate) struct AppState {
    /// Per-process UUID generated at `AppState::new_with_paths` boot.
    /// Carried on every `StateResponse` and `HealthResponse` so clients
    /// can distinguish "revision decreased because the server just
    /// restarted" from "revision decreased because this response is
    /// stale". The frontend's `shouldAdoptSnapshotRevision` uses a
    /// mismatch between this id and its `lastSeenServerInstanceIdRef`
    /// as the signal to accept a revision downgrade.
    pub(crate) server_instance_id: String,
    pub(crate) default_workdir: String,
    /// Local HTTP origin for agent-facing bridge processes that need to call
    /// TermAl's own API. Set after the server socket is bound; falls back to
    /// TERMAL_BASE_URL/TERMAL_PORT for tests and non-server modes.
    pub(crate) local_http_base_url: Arc<Mutex<Option<String>>>,
    pub(crate) persistence_path: Arc<PathBuf>,
    /// Durable neutral coordination mailboxes use their own long-lived SQLite
    /// connection. This deliberately bypasses the asynchronous state persist
    /// worker: append/fetch/ack must remain available during and after worker
    /// shutdown, and committed messages must be visible before any receiver
    /// wake-up is attempted.
    pub(crate) mailbox_store: Arc<MailboxStore>,
    /// Level-triggered sibling of the mailbox store for versioned,
    /// per-repository coordination facts. Mailbox and board each keep one
    /// long-lived connection to coordination.sqlite and share that file's FIFO
    /// writer admission, isolated from termal.sqlite session/transcript writes.
    /// Board writes never wake a session — activation stays mailbox-only.
    pub(crate) coordination_board_store: Arc<CoordinationBoardStore>,
    pub(crate) orchestrator_templates_path: Arc<PathBuf>,
    /// Must not be held at the same time as `self.inner`; template file I/O happens
    /// outside the main state mutex so we never invert lock ordering.
    pub(crate) orchestrator_templates_lock: Arc<Mutex<()>>,
    /// Must not be held at the same time as `self.inner`; review file I/O stays
    /// outside the main state mutex so disk writes do not stall unrelated state work.
    pub(crate) review_documents_lock: Arc<Mutex<()>>,
    /// Serializes carried-run polls (`EngramHost::test_run_tick`), so a
    /// terminal read made off the state lock is stored before another poll,
    /// or a checkpoint's settlement after it, can read the same run.
    pub(crate) engram_carried_poll_lock: Arc<Mutex<()>>,
    /// The automatic Engram admission retries in flight host-wide, under
    /// their cap (`engram_retry_schedule.rs`).
    pub(crate) runtime_retry_slots: RuntimeRetrySlots,
    /// Where published snapshots and deltas go: the ordered stream
    /// `/api/events` reads, plus per-kind observer channels in test builds.
    pub(crate) state_broadcast_senders: StateBroadcastSenders,
    pub(crate) file_events: broadcast::Sender<String>,
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) file_events_revision: Arc<AtomicU64>,
    /// Background persistence channel. `persist_internal_locked` sends a
    /// pre-cloned `PersistedState` snapshot through this channel; a
    /// dedicated thread serializes it to JSON and writes the file so the
    /// state mutex is never held during I/O.
    pub(crate) persist_tx: mpsc::Sender<PersistRequest>,
    pub(crate) persist_observer: Arc<PersistObserver>,
    /// Handle to the background persist thread. Wrapped in
    /// `Arc<Mutex<Option<_>>>` so that:
    ///   - `AppState` stays `Clone` (the handle is shared, not duplicated),
    ///   - exactly one shutdown caller can `take()` the handle and join,
    ///   - subsequent shutdown calls block behind the join owner and then
    ///     become a safe no-op.
    /// Populated by `AppState::new_with_paths` after spawning the thread.
    /// `None` for test-only constructors that don't spawn the thread —
    /// `shutdown_persist_blocking` then has nothing to wait on.
    pub(crate) persist_thread_handle: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// `true` while the background persist worker may still be alive and able
    /// to drain `PersistRequest::Delta` signals; flipped to `false` by
    /// `shutdown_persist_blocking` only AFTER the worker thread has joined.
    /// Read by `commit_delta_locked` (and any other path that stamps a
    /// mutation without sending its own persist signal) to switch from
    /// the async-worker path to a synchronous full-state JSON write. Once the
    /// flag is false, the worker is demonstrably gone and cannot race the
    /// synchronous fallback with its final drain/write; no future worker drain
    /// can persist a mutation stamped after that point.
    /// See bugs.md "Post-shutdown persistence writes still leave a
    /// post-collection-pre-join window".
    pub(crate) persist_worker_alive: Arc<std::sync::atomic::AtomicBool>,
    /// Graceful-shutdown signal for long-lived SSE streams. Triggered by
    /// `main.rs::shutdown_signal` (Ctrl+C / SIGTERM) before the
    /// `axum::serve` graceful-shutdown future resolves; SSE handlers
    /// `subscribe()` and exit their `tokio::select!` loops when the
    /// channel value flips to `true`. Without this, `with_graceful_shutdown`
    /// would block forever waiting for SSE streams to finish — the
    /// broadcast receivers in `state_events` only return `Closed` when the
    /// last `AppState` clone drops, but `shutdown_state` keeps a clone
    /// alive for the post-serve persist drain.
    ///
    /// Uses `tokio::sync::watch` rather than `tokio::sync::Notify` because
    /// `Notify::notify_waiters()` is *not* sticky: a waiter that subscribes
    /// after the notification fires never wakes. With `watch`, the value
    /// `true` is durable — a receiver constructed *after* `send(true)` can
    /// `borrow_and_update()` and see it immediately, so `/api/events`
    /// connections accepted just before Ctrl+C are guaranteed to observe
    /// the shutdown signal regardless of registration ordering. See
    /// bugs.md "One-shot SSE shutdown notification can be missed before
    /// waiter registration" / "Graceful shutdown blocks forever waiting
    /// for SSE streams to drain".
    pub(crate) shutdown_signal_tx: Arc<tokio::sync::watch::Sender<bool>>,
    pub(crate) runtime_observation_lifetime: Arc<EngramRuntimeObservationLifetime>,
    /// Host-owned capture handles and cancellation, outside the state mutex.
    pub(crate) capture_lifetime: Arc<CaptureLifetime>,
    /// Background SSE state/delta broadcast mailbox. `publish_snapshot`
    /// stores a pre-built `StateResponse` here; a dedicated thread serializes
    /// snapshots to JSON and forwards each queued item to the matching SSE
    /// broadcast channel, so the state mutex is never held during the
    /// O(sessions × messages) serialization pass. Consecutive snapshots
    /// coalesce, but retained snapshot-before-delta ordering is preserved so
    /// browsers do not observe delta N+1 before state N. The queue is bounded
    /// and drops the oldest pending work when full, matching the bounded nature
    /// of the downstream broadcast channels without blocking producers that
    /// still hold the state mutex. Dropped deltas surface as ordinary revision
    /// gaps to clients, which then repair from `/api/state`.
    pub(crate) state_broadcast_mailbox: Option<Arc<StateBroadcastMailbox>>,
    pub(crate) state_broadcast_worker: Option<Arc<StateBroadcastWorker>>,
    /// Owns the optional in-process Telegram relay runtime for this app
    /// instance. Kept outside `StateInner` because it is live process state
    /// (thread handle, shutdown flag, status), not durable app data.
    pub(crate) telegram_relay_runtime: Arc<Mutex<TelegramRelayRuntime>>,
    /// Lazily created shared Codex app-server reused across Codex sessions.
    pub(crate) shared_codex_runtime: Arc<Mutex<Option<SharedCodexRuntime>>>,
    /// The app-server for read-only-sandbox Codex sessions on Windows
    /// (`SharedCodexProfile::ReadOnlySandbox`), started on first demand like
    /// the default one.
    pub(crate) shared_codex_read_only_runtime: Arc<Mutex<Option<SharedCodexRuntime>>>,
    /// A separate Windows workspace-write worker process; its lifecycle must
    /// not retire read-only reviewers or default-profile root threads.
    pub(crate) shared_codex_workspace_write_runtime: Arc<Mutex<Option<SharedCodexRuntime>>>,
    /// Delegation-owned app-servers by runtime id, with the profile whose
    /// Codex home they use (`owned_codex_server.rs`).
    pub(crate) owned_codex_servers: OwnedCodexServerRegistry,
    /// Runtime ids whose exit cascade has already been claimed. Several
    /// worker threads may report one shared-process death; only the first may
    /// terminalize sessions or rebind Engram.
    pub(crate) shared_codex_exit_claims: Arc<Mutex<HashSet<String>>>,
    /// Whether this state may launch real local agent subprocesses. Production
    /// states enable spawning; lightweight test states disable it so ordinary
    /// state-transition tests cannot accidentally retain real agent runtimes.
    pub(crate) agent_runtime_spawning_enabled: bool,
    #[cfg(test)]
    pub(crate) test_acp_runtime_overrides: Arc<Mutex<Vec<TestAcpRuntimeOverride>>>,
    #[cfg(test)]
    pub(crate) test_agent_setup_failures: Arc<Mutex<Vec<(Agent, String)>>>,
    /// Cached app-level agent readiness lives outside `self.inner` so full
    /// snapshots can clone the latest value without filesystem work under the
    /// main state mutex.
    pub(crate) agent_readiness_cache: Arc<RwLock<AgentReadinessCache>>,
    /// Serializes cache refreshes so concurrent snapshot requests do not all
    /// repeat the same readiness filesystem probes.
    pub(crate) agent_readiness_refresh_lock: Arc<Mutex<()>>,
    /// Owns SSH-backed remote connections and their event bridges.
    pub(crate) remote_registry: Arc<RemoteRegistry>,
    /// Runtime-only handles; the supervisor owns insert/remove and shutdown.
    pub(crate) federation_peer_handles: FederationPeerHandles,
    /// Observational cache; never persisted or used for execution admission.
    pub(crate) host_utilization: Arc<host_utilization::Cache>,
    pub(crate) federation_executor_wake_signal: Arc<FederationExecutorWakeSignal>,
    pub(crate) test_run_phase_traces: TestRunPhaseTraces,
    /// Tracks the newest `_sseFallback` revision already recovered per remote
    /// so duplicate or older fallback events do not trigger redundant
    /// blocking `/api/state` fetches.
    pub(crate) remote_sse_fallback_resynced_revision: Arc<Mutex<HashMap<String, u64>>>,
    /// Exact remote-delta payload identities for every successfully applied
    /// inbound delta. Suppresses same-revision payload-identical replays from a
    /// misbehaving remote/SSE retry; sibling same-revision deltas with different
    /// payloads still apply. Per-remote entries are cleared on event-stream
    /// continuity loss.
    pub(crate) remote_delta_replay_cache: Arc<Mutex<RemoteDeltaReplayCache>>,
    /// Remote session transcript hydrations currently being fetched because a
    /// delta reached an unloaded proxy session. The key includes the authority
    /// generation so endpoint B is never blocked by endpoint A's retired
    /// request for the same remote/session ids. Same-generation duplicates skip
    /// the narrow unloaded-transcript path and are left un-replayed until a
    /// later event or the in-flight fetch repairs the transcript.
    pub(crate) remote_delta_hydrations_in_flight: Arc<Mutex<HashSet<RemoteDeltaHydrationKey>>>,
    /// Remote register/upgrade lifecycle actions currently running. The UI
    /// disables buttons while pending, but this backend guard is the
    /// correctness boundary for duplicate browser tabs or direct API retries.
    pub(crate) remote_lifecycle_actions_in_flight: Arc<Mutex<HashSet<String>>>,
    pub(crate) terminal_local_command_semaphore: Arc<tokio::sync::Semaphore>,
    pub(crate) terminal_remote_command_semaphore: Arc<tokio::sync::Semaphore>,
    pub(crate) stopping_orchestrator_ids: Arc<Mutex<HashSet<String>>>,
    pub(crate) stopping_orchestrator_session_ids: Arc<Mutex<HashMap<String, HashSet<String>>>>,
    pub(crate) inner: Arc<StateMutex<StateInner>>,
    /// Test directories live until the final AppState clone drops. This field
    /// stays last so every store, SQLite connection, and state handle above is
    /// closed before Windows attempts recursive removal.
    #[cfg(test)]
    pub(crate) test_temp_root: Option<Arc<TestTempRoot>>,
}

#[cfg(test)]
impl AppState {
    pub(crate) fn test_temp_root_path(&self) -> Option<&FsPath> {
        self.test_temp_root.as_deref().map(TestTempRoot::path)
    }
}

pub(crate) const SESSION_NOT_RUNNING_CONFLICT_MESSAGE: &str = "session is not currently running";
pub(crate) const TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT: usize = 4;
pub(crate) const TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT: usize = 4;
pub(crate) const AGENT_READINESS_CACHE_TTL: Duration = Duration::from_secs(5);
pub(crate) const ACTIVE_TURN_FILE_CHANGE_GRACE: Duration = Duration::from_millis(750);

#[derive(Clone)]
pub(crate) struct AgentReadinessCache {
    pub(crate) snapshot: Vec<AgentReadiness>,
    pub(crate) expires_at: std::time::Instant,
    pub(crate) invalidated: bool,
    // Runtime observations, independent of the five-second installation probe.
    // Claude then Codex; never persisted or reconstructed from transcript prose.
    pub(crate) evaluator_usage_limits: [Option<std::time::Instant>; 2],
    #[cfg(test)]
    pub(crate) phase_sink: Option<mpsc::Sender<TestApiPhaseObservation>>,
}

impl AgentReadinessCache {
    pub(crate) fn fresh(snapshot: Vec<AgentReadiness>) -> Self {
        Self {
            snapshot,
            expires_at: std::time::Instant::now() + AGENT_READINESS_CACHE_TTL,
            invalidated: false,
            evaluator_usage_limits: [None; 2],
            #[cfg(test)]
            phase_sink: None,
        }
    }

    pub(crate) fn needs_refresh(&self, now: std::time::Instant) -> bool {
        self.invalidated || now >= self.expires_at
    }

    pub(crate) fn refresh_probe(&mut self, snapshot: Vec<AgentReadiness>) {
        self.snapshot = snapshot;
        self.expires_at = std::time::Instant::now() + AGENT_READINESS_CACHE_TTL;
        self.invalidated = false;
    }
}

/// Disposable attribution extends the readiness cache's existing owner.
#[cfg(test)]
pub(crate) struct TestApiPhaseObservation {
    pub(crate) phase: &'static str,
    pub(crate) session_id: Option<String>,
    pub(crate) thread_id: std::thread::ThreadId,
    pub(crate) started: std::time::Instant,
    pub(crate) elapsed: Duration,
}

#[cfg(test)]
pub(crate) struct TestApiPhaseTimer {
    pub(crate) sink: mpsc::Sender<TestApiPhaseObservation>,
    pub(crate) observation: TestApiPhaseObservation,
}

#[cfg(test)]
impl Drop for TestApiPhaseTimer {
    fn drop(&mut self) {
        self.observation.elapsed = self.observation.started.elapsed();
        let _ = self.sink.send(TestApiPhaseObservation {
            phase: self.observation.phase,
            session_id: self.observation.session_id.take(),
            thread_id: self.observation.thread_id,
            started: self.observation.started,
            elapsed: self.observation.elapsed,
        });
    }
}

pub(crate) fn fresh_agent_readiness_cache(default_workdir: &str) -> AgentReadinessCache {
    AgentReadinessCache::fresh(collect_agent_readiness(default_workdir))
}

/// Holds stop session options.
#[derive(Clone)]
pub(crate) struct StopSessionOptions {
    pub(crate) dispatch_queued_prompts_on_success: bool,
    pub(crate) pause_automatic_resumes_on_success: bool,
    pub(crate) orchestrator_stop_instance_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeStopOwnerKind {
    UserStop,
    #[cfg(windows)]
    DedicatedReset,
    EngramMcpRevocation,
    LostRuntimeTerminalization,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeStopOwner {
    pub(crate) kind: RuntimeStopOwnerKind,
    pub(crate) token: Option<RuntimeToken>,
    pub(crate) generation: u64,
}

impl Default for StopSessionOptions {
    /// Builds the default value.
    fn default() -> Self {
        Self {
            dispatch_queued_prompts_on_success: false,
            pause_automatic_resumes_on_success: true,
            orchestrator_stop_instance_id: None,
        }
    }
}

/// Handles bootstrap default local state.
pub(crate) fn bootstrap_default_local_state(default_workdir: &str) -> StateInner {
    let mut inner = StateInner::new();
    let default_project =
        inner.create_project(None, default_workdir.to_owned(), default_local_remote_id());
    inner.create_session(
        Agent::Codex,
        Some("Codex Live".to_owned()),
        default_workdir.to_owned(),
        Some(default_project.id.clone()),
        None,
    );
    inner.create_session(
        Agent::Claude,
        Some("Claude Live".to_owned()),
        default_workdir.to_owned(),
        Some(default_project.id.clone()),
        None,
    );
    inner
}

/// Describes whether a runtime-gated mutation actually applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub(crate) enum RuntimeMatchOutcome {
    Applied,
    SessionMissing,
    RuntimeMismatch,
}

/// Normalizes remote configs.
pub(crate) fn normalize_remote_configs(
    remotes: Vec<RemoteConfig>,
) -> Result<Vec<RemoteConfig>, ApiError> {
    let mut normalized = vec![RemoteConfig::local()];
    let mut seen_ids = HashSet::from([default_local_remote_id()]);

    for remote in remotes {
        let id = remote.id.trim();
        validate_remote_id_value(id)?;
        if id.eq_ignore_ascii_case(LOCAL_REMOTE_ID) {
            continue;
        }
        if !seen_ids.insert(id.to_owned()) {
            return Err(ApiError::bad_request(format!("duplicate remote id `{id}`")));
        }

        let name = remote.name.trim();
        if name.is_empty() {
            return Err(ApiError::bad_request(format!(
                "remote `{id}` must have a name"
            )));
        }

        match remote.transport {
            RemoteTransport::Local => {
                return Err(ApiError::bad_request(format!(
                    "remote `{id}` cannot use local transport"
                )));
            }
            RemoteTransport::Ssh => {
                let host = normalized_remote_ssh_host(&remote)?;
                normalized.push(RemoteConfig {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    transport: RemoteTransport::Ssh,
                    enabled: remote.enabled,
                    host: Some(host),
                    port: Some(remote.port.unwrap_or(DEFAULT_SSH_REMOTE_PORT)),
                    user: normalized_remote_ssh_user(&remote)?,
                });
            }
        }
    }

    Ok(normalized)
}

/// Validates persisted remote configs.
pub(crate) fn validate_persisted_remote_configs(
    remotes: Vec<RemoteConfig>,
) -> Result<Vec<RemoteConfig>> {
    normalize_remote_configs(remotes).map_err(|err| anyhow!(err.message))
}

/// Normalizes local user facing path.
pub(crate) fn normalize_local_user_facing_path(path: &str) -> String {
    normalize_user_facing_path(FsPath::new(path))
        .to_string_lossy()
        .into_owned()
}

/// Normalizes workspace layout paths.
pub(crate) fn normalize_workspace_layout_paths(layout: &mut WorkspaceLayoutDocument) {
    let Some(panes) = layout
        .workspace
        .get_mut("panes")
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    for pane in panes {
        normalize_workspace_layout_path_field(pane, "sourcePath");

        let Some(tabs) = pane.get_mut("tabs").and_then(Value::as_array_mut) else {
            continue;
        };

        for tab in tabs {
            match tab.get("kind").and_then(Value::as_str) {
                Some("source") => normalize_workspace_layout_path_field(tab, "path"),
                Some("filesystem") => normalize_workspace_layout_path_field(tab, "rootPath"),
                Some("gitStatus") | Some("instructionDebugger") => {
                    normalize_workspace_layout_path_field(tab, "workdir")
                }
                Some("diffPreview") => normalize_workspace_layout_path_field(tab, "filePath"),
                _ => {}
            }
        }
    }
}

/// Normalizes workspace layout path field.
fn normalize_workspace_layout_path_field(object: &mut Value, key: &str) {
    let Some(field) = object.get_mut(key) else {
        return;
    };
    let Some(path) = field.as_str() else {
        return;
    };
    *field = Value::String(normalize_local_user_facing_path(path));
}

/// Builds sorted workspace layout summaries.
pub(crate) fn collect_workspace_layout_summaries<'a>(
    layouts: impl Iterator<Item = &'a WorkspaceLayoutDocument>,
) -> Vec<WorkspaceLayoutSummary> {
    let mut workspaces = layouts
        .map(|layout| WorkspaceLayoutSummary {
            id: layout.id.clone(),
            label: layout.label.clone(),
            revision: layout.revision,
            updated_at: layout.updated_at.clone(),
            control_panel_side: layout.control_panel_side,
            light_theme_id: layout.light_theme_id.clone(),
            dark_theme_id: layout.dark_theme_id.clone(),
            theme_mode: layout.theme_mode.clone(),
            style_id: layout.style_id.clone(),
            font_size_px: layout.font_size_px,
            editor_font_size_px: layout.editor_font_size_px,
            density_percent: layout.density_percent,
        })
        .collect::<Vec<_>>();
    workspaces.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    workspaces
}

#[derive(Default)]
pub(crate) struct EngramProjectResetFences {
    pub(crate) next_generation: u64,
    pub(crate) owners: HashMap<String, u64>,
    last_generation: HashMap<String, u64>,
    /// Exact live connection-transition claim, never restored from history.
    pub(crate) claimed_at: HashMap<String, EngramProjectResetClaim>,
}

impl EngramProjectResetFences {
    pub(crate) fn contains(&self, project_id: &str) -> bool {
        self.owners.contains_key(project_id)
    }

    pub(crate) fn last_generation(&self, project_id: &str) -> u64 {
        self.last_generation.get(project_id).copied().unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn claim(&mut self, project_id: &str) -> Option<u64> {
        self.claim_with_clock(project_id, EngramBudgetClock::default())
    }

    pub(crate) fn claim_with_clock(
        &mut self,
        project_id: &str,
        clock: EngramBudgetClock,
    ) -> Option<u64> {
        if self.contains(project_id) {
            return None;
        }
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let generation = self.next_generation;
        self.owners.insert(project_id.to_owned(), generation);
        self.last_generation
            .insert(project_id.to_owned(), generation);
        self.claimed_at.insert(
            project_id.to_owned(),
            EngramProjectResetClaim::new(generation, clock),
        );
        Some(generation)
    }

    /// When the fence held for `project_id` was claimed, if one is held.
    #[cfg(test)]
    pub(crate) fn claimed_at(&self, project_id: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        self.claimed_at
            .get(project_id)
            .map(|claim| claim.claimed_at)
    }

    pub(crate) fn is_owned_by(&self, project_id: &str, generation: u64) -> bool {
        self.owners.get(project_id).copied() == Some(generation)
    }

    pub(crate) fn release(&mut self, project_id: &str, generation: u64) -> bool {
        if !self.is_owned_by(project_id, generation) {
            return false;
        }
        self.owners.remove(project_id);
        self.claimed_at.remove(project_id);
        true
    }
}

/// Represents state inner.
pub(crate) struct StateInner {
    pub(crate) metadata_predecessor: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) metadata_watermarks: Arc<Mutex<PersistMetadataWatermarks>>,
    /// Process-local projections; a restored/replaced StateInner starts cold.
    /// Access only under StateInner, never from the broadcaster worker.
    pub(crate) state_projection_cache: Mutex<StateProjectionCache>,
    // Runtime identity of this whole value, refreshed by construction/restore.
    // Never persisted; replacing it cannot join observations across reloads.
    pub(crate) state_lock_epoch: u64,
    pub(crate) federation: FederationState,
    pub(crate) federation_durable: bool,
    pub(crate) federation_durability_diagnostics: FederationDurabilityDiagnostics,
    pub(crate) federation_continuity_lost: bool,
    pub(crate) federation_attestation: HashMap<String, FederationAttestationObservation>,
    pub(crate) codex: CodexState,
    /// Process/runtime side of the optional Engram host adapter. Callers may
    /// clone this handle while holding `inner`, but must release the state
    /// mutex before invoking any control operation.
    pub(crate) engram_host_adapter: Arc<EngramHostAdapter>,
    /// One clock domain for control/authority budgets; runtime-only and shared
    /// with targets and recovery workers. Fixtures opt in before operations.
    /// Read only through `StateInner::engram_budget_clock_snapshot`.
    pub(crate) engram_budget_clock: EngramBudgetClock,
    /// Whether a fixture chose this state's clock (a scripted clock, or Real
    /// on purpose); a later choice keeps it (`engram_budget.rs`).
    #[cfg(test)]
    pub(crate) engram_budget_clock_selected: bool,
    /// Snapshots taken of the clock, so a fixture that chooses one after a
    /// target or worker already holds the old clock fails at once.
    #[cfg(test)]
    pub(crate) engram_budget_clock_snapshots: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) test_engram_authority_ack_boundary: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// Real time a delegation child's first turn waits before its admission,
    /// after the child's best-effort bind (`start_delegation_child_turn`): a
    /// scheduler delay a test can apply on purpose.
    #[cfg(test)]
    pub(crate) test_delegation_child_admission_stall: Option<Duration>,
    /// Runtime-only cache of projects whose non-empty `.engram-project`
    /// marker was confirmed off the global state mutex. Hot locked paths may
    /// consult this set but must never touch the filesystem themselves.
    pub(crate) engram_declared_project_ids: HashSet<String>,
    /// Projects whose `.engram-project` marker has been observed in this
    /// process. The first observation seeds the cache; later changes reset
    /// already-created runtimes so their MCP configuration follows the marker.
    pub(crate) engram_declaration_checked_project_ids: HashSet<String>,
    /// Runtime-only project fence used while Engram connection settings drain
    /// the old sidecars. It prevents newly-created delegations from binding to
    /// the old connection between the reset snapshot and commit.
    pub(crate) engram_project_resets: EngramProjectResetFences,
    pub(crate) preferences: AppPreferences,
    /// Runtime retry marker for settings mutations that changed memory but
    /// failed synchronous persistence. An identical request must retry the
    /// write instead of reporting success merely because the values already
    /// match memory.
    pub(crate) settings_persist_dirty: bool,
    /// Tracks whether the dirty settings include remote routing so repeated
    /// failures keep the operator warning specific and accurate.
    pub(crate) remote_settings_persist_dirty: bool,
    /// A remote delta mutated in-memory state but its synchronous persistence
    /// attempt returned an error. Exact redelivery may now be a semantic no-op,
    /// so those paths must retry the full snapshot before advancing remote
    /// revision/replay bookkeeping.
    pub(crate) remote_delta_persist_dirty: bool,
    pub(crate) revision: u64,
    pub(crate) next_session_number: usize,
    pub(crate) next_message_number: u64,
    /// Stable project records used by local and remote session routing.
    pub(crate) projects: PersistTracked<Vec<Project>>,
    /// Host-level fail-closed ledger for Engram work-authority tuples that
    /// TermAl retired. Unlike project settings, this survives project deletion
    /// so recreating the same Engram store cannot reuse a retired credential.
    pub(crate) engram_retired_work_authority_grants:
        PersistTracked<Vec<EngramRetiredWorkAuthorityGrant>>,
    pub(crate) engram_session_obligations: EngramSessionObligationLedger,
    // Lazily activated import-reconciliation journal: NotActivated until the
    // first enrollment or hold intent (persist_import_reconciliation.rs).
    // Tracked so an unchanged journal is omitted from delta captures.
    pub(crate) engram_import_reconciliation: PersistTracked<EngramImportReconciliationState>,
    /// The source roots agents named for claimed Engram work
    /// (`engram_source_roots.rs`), persisted so a restart keeps them.
    pub(crate) engram_work_source_roots: PersistTracked<Vec<EngramWorkSourceRoot>>,
    pub(crate) engram_named_root_journal: PersistTracked<Vec<EngramNamedRootJournal>>,
    /// Per-fixture capacity for bounded lifecycle witnesses; never persisted.
    #[cfg(test)]
    pub(crate) test_engram_named_root_journal_limit: Option<usize>,
    pub(crate) engram_work_naming_history: PersistTracked<Vec<EngramWorkNamingHistory>>,
    /// The operator releases of retained named roots, newest last
    /// (`engram_named_root_release.rs`), persisted as their audit.
    pub(crate) engram_named_root_release_audit: PersistTracked<Vec<EngramNamedRootReleaseAudit>>,
    pub(crate) engram_source_sightings: PersistTracked<Vec<EngramSourceSightingOwner>>,
    pub(crate) engram_root_read_cursor: BTreeMap<EngramAuthorityStoreKey, String>,
    pub(crate) engram_authority_read_cursor: BTreeMap<EngramAuthorityStoreKey, String>,
    /// Bounded background reads of retained runs at startup and turn
    /// opening (`engram_retained_run_settlement.rs`). Not persisted.
    pub(crate) engram_retained_run_reads: EngramRetainedRunReads,
    pub(crate) engram_root_reclamation: EngramRootReclamationSchedule,
    /// The generation the last new source-root name got; the next is one
    /// more, so no generation is given twice, even after a clear. Persisted
    /// (`PersistedState`), and on restore raised to the highest generation
    /// stored, the orphans' included. An evaluation's root check
    /// (`AcceptanceEvaluationSourceRoot::still_named`) compares the root as
    /// well as the generation.
    pub(crate) engram_source_root_generation: u64,
    /// Durable outbox of project scopes whose coordination-board data must be
    /// fenced and removed after the project deletion reaches termal.sqlite.
    /// The dedicated cleanup worker removes an item in memory only after the
    /// fence/cascade succeeds, then wakes primary persistence to durably clear
    /// it; a crash leaves the on-disk item for the next boot.
    pub(crate) pending_coordination_scope_deletions: PersistTracked<BTreeSet<String>>,
    /// Durable outbox of deleted projects whose project-default response-board
    /// tabs must become ordinary custom tabs. Values retain the project's last
    /// live name so an idempotent retry cannot fall back to the creation-time
    /// tab label after the project record has disappeared.
    pub(crate) pending_response_board_project_detachments: PersistTracked<BTreeMap<String, String>>,
    pub(crate) ignored_discovered_codex_thread_ids: PersistTracked<BTreeSet<String>>,
    /// Broad remote state is metadata-only. Focused bounded-tail responses use
    /// `remote_session_transcript_applied_revisions` so a same-revision delta is
    /// suppressed only for the session whose retained suffix was materialized.
    /// Tracks the latest remote revision applied for each mirrored remote so
    /// stale snapshots and deltas cannot roll local proxy state backward.
    pub(crate) remote_applied_revisions: HashMap<String, u64>,
    /// Tracks the latest broad full-state snapshot applied for each mirrored
    /// remote. Cleared with `remote_applied_revisions` and the per-remote replay
    /// cache when event-stream continuity is lost.
    pub(crate) remote_snapshot_applied_revisions: HashMap<String, u64>,
    /// Tracks focused bounded-tail materialization per remote session.
    pub(crate) remote_session_transcript_applied_revisions: HashMap<String, HashMap<String, u64>>,
    /// Session records carry the serializable session plus runtime-only state.
    pub(crate) sessions: SessionSequence,
    /// Durable session rows isolated during startup validation.
    ///
    /// These records are deliberately absent from `sessions` so corrupted
    /// payloads cannot appear healthy, but full-snapshot fallback persistence
    /// must preserve their SQLite rows for recovery instead of treating them as
    /// user-deleted sessions.
    pub(crate) quarantined_persisted_session_ids: BTreeSet<String>,
    /// Runtime instances for orchestrator templates live beside ordinary sessions.
    pub(crate) orchestrator_instances: PersistTracked<Vec<OrchestratorInstance>>,
    /// Durable parent-child delegation links for ordinary sessions.
    pub(crate) delegations: Vec<DelegationRecord>,
    /// Runtime-only completeness of the full durable delegation inventory.
    pub(crate) delegation_inventory: DelegationInventoryState,
    /// In-process admission owners. Terminal records remain durable and readable
    /// until a reserved follow-up reaches prompt admission under the state lock.
    /// Values retain the last Author::You id before admission (None for an
    /// empty transcript). Cancellation blocks admission but retains cleanup
    /// ownership until release. A new user boundary or queue entry transfers
    /// ownership from pre-prompt archive compensation to the new attempt.
    pub(crate) delegation_followup_admissions: HashMap<String, FollowupAdmissionReservation>,
    /// Evaluator delegations with an acceptance submission in progress, from
    /// its admission to its last durability acknowledgement. Memory only: a
    /// restart has no request in flight, and the persisted `pending` already
    /// carries what such a request left open.
    // The value coalesces a child completion observed before this submission
    // owner's final durability acknowledgement; no separate refresh queue.
    pub(crate) acceptance_evaluation_submissions_in_flight: HashMap<String, bool>,
    /// Attempt-scoped read cursors, replay cache and actual-I/O allowance.
    /// A restart loses these capabilities; it never resets an old allowance.
    pub(crate) acceptance_evidence_recovery:
        HashMap<String, Arc<Mutex<AcceptanceEvidenceRecovery>>>,
    /// The parent packet's last failed publication of a terminal review
    /// attempt, keyed by (canonical parent, delegation, attempt). It is served
    /// instead of a new retention until the delegation record changes, which
    /// removes it, as do removing the delegation or its parent session. Memory
    /// only, never persisted: a restart retries once.
    pub(crate) review_packet_retention_failures:
        HashMap<(String, String, u32), ReviewPacketRetentionFailure>,
    /// Threads taking an acceptance evaluation's source revision that are
    /// still alive, abandoned ones included (`bounded_source_capture`).
    /// Memory only, one count for the host.
    pub(crate) acceptance_source_captures_live: Arc<std::sync::atomic::AtomicUsize>,
    /// Threads taking a turn's begin or closing source basis that are still
    /// alive, abandoned ones included. Memory only, one count for the host.
    pub(crate) engram_turn_basis_captures_live: Arc<std::sync::atomic::AtomicUsize>,
    /// Per-state test dependency for logical capture completion/absence.
    /// Never persisted; production always uses the bounded filesystem owner.
    #[cfg(test)]
    pub(crate) test_engram_source_capture:
        Option<Arc<dyn Fn(&EngramBasisPlace, Duration) -> EngramSourceCapture + Send + Sync>>,
    #[cfg(test)]
    pub(crate) test_observation_source_owner: Option<
        Arc<
            dyn Fn(
                    &FsPath,
                    &FsPath,
                    &[String],
                    std::time::Instant,
                    usize,
                ) -> Result<std::process::Output>
                + Send
                + Sync,
        >,
    >,
    #[cfg(test)]
    pub(crate) test_declared_run_snapshot: Option<
        Arc<dyn Fn(&FsPath, std::time::Instant) -> Result<DeclaredRunSnapshot> + Send + Sync>,
    >,
    #[cfg(test)]
    pub(crate) test_declared_run_clock: Option<Arc<dyn Fn() -> std::time::Instant + Send + Sync>>,
    #[cfg(test)]
    pub(crate) test_declared_run_source: Option<
        Arc<
            dyn Fn(
                    &EngramDeclaredBinding,
                    &EngramSourceOwnerPin,
                    std::time::Instant,
                ) -> Result<DeclaredRunSource>
                + Send
                + Sync,
        >,
    >,
    #[cfg(test)]
    pub(crate) test_native_source_owner_metadata: Option<
        Arc<
            dyn Fn(
                    &AppState,
                    &str,
                    std::time::Instant,
                ) -> std::result::Result<
                    (EngramSourceOwnerPin, EngramSourceOwnerAuthority),
                    ApiError,
                > + Send
                + Sync,
        >,
    >,
    #[cfg(test)]
    pub(crate) test_acceptance_source_owner_capture: Option<
        Arc<
            dyn Fn(
                    &FsPath,
                    &FsPath,
                    &[String],
                    std::time::Instant,
                    usize,
                ) -> Result<std::process::Output>
                + Send
                + Sync,
        >,
    >,
    /// Threads checking a path named as a work's source root that are still
    /// alive, abandoned ones included (`engram_run_source_root_validation_within`).
    /// Memory only, one count for the host.
    pub(crate) engram_source_root_validations_live: Arc<std::sync::atomic::AtomicUsize>,
    /// The test launcher's runs, mirrored from their run directories by the
    /// rescan in `test_runs.rs`. Memory only: disk stays the record.
    pub(crate) test_runs: TestRunIndex,
    // Retained history with maintained candidate indexes
    // (test_run_history_store.rs); mutations go through their helpers.
    pub(crate) test_run_registrations: TestRunRegistrationStore,
    pub(crate) test_run_observations: TestRunObservationStore,
    pub(crate) test_run_compacted: TestRunCompactedStore,
    /// Process-local: run id to the body digest of a held receipt whose
    /// settlement found no launch observation of the run carrying its
    /// records (`EngramVerificationRecordSettlement::NoLaunchRow`); a run wait
    /// resumes on it naming the received records. Removed once a launch
    /// observation carries them. A restarted host finds it again on the
    /// producer's first look at the held receipt.
    pub(crate) test_run_unassociated_receipts: HashMap<String, String>,
    /// Process-local: run id to what the producer's latest look at the run's
    /// record came to, a pending reason or no blocker
    /// (`AppState::prepare_engram_verification_record` and the send step).
    /// A run wait decides from it, never from the state file beside the run,
    /// which can be older or unwritten. A restarted host writes it again on
    /// the producer's first look; until then a run wait keeps waiting.
    pub(crate) test_run_record_pending: HashMap<String, EngramVerificationRecordLook>,
    /// Process-local: the place the producer's next look at any run's record
    /// takes, counted up as each look begins and before it reads a file. A
    /// run wait notes it when it marks a run, and counts a look that found no
    /// record only from a look begun at or after that place
    /// (test_run_wait_records.rs).
    pub(crate) test_run_look_sequence: u64,
    /// Process-local: how many rescans have installed their index since this
    /// host started (`refresh_test_runs`).
    pub(crate) test_run_scan_generation: u64,
    /// Process-local: run id to `test_run_scan_generation` when the run's
    /// verification record became final. A run wait takes the verdict of a run
    /// the index has not listed from its final record once a full rescan
    /// begun after that has completed (test_run_waits.rs). A restarted host
    /// counts from its first rescans.
    pub(crate) test_run_record_final_at_scan: HashMap<String, u64>,
    /// Process-local: runs whose record body a producer look of this process
    /// froze (ready to send, or held with its receipt), so a send of it may be
    /// in flight from memory. A run wait follows such a run's record even when
    /// its verdict is unknown, as when its directory was removed
    /// (test_run_wait_records.rs).
    pub(crate) test_run_record_bodies_built: HashSet<String>,
    /// Process-local: run id to the place of a producer look that has begun
    /// but not yet published what it came to. A run wait's first resume
    /// waits for it before deciding whether a record of the run may be in
    /// flight (test_run_wait_records.rs).
    pub(crate) test_run_record_looks_in_flight: HashMap<String, u64>,
    /// Process-local: run id to the place of the latest producer look that
    /// published what it came to. A run wait that deferred for a look in
    /// flight decides once that look has published (test_run_wait_records.rs).
    pub(crate) test_run_record_look_published: HashMap<String, u64>,
    /// Process-local: settled runs whose Settled state file beside the run
    /// (the launcher summary's copy) could not be written; the producer's
    /// ticks write it again. A loaded host starts with every settled run here,
    /// so a copy left behind before a restart is repaired too.
    pub(crate) test_run_unmirrored_settlements: BTreeSet<String>,
    /// When this host first ran with test-run cards; runs that started
    /// earlier never get one (`test_run_cards.rs`). Persisted, never derived
    /// from "now" again once stored.
    pub(crate) test_run_cards_epoch: Option<String>,
    /// Each run's card, by run id: the owner session and message. Persisted;
    /// the transcript stays the render source.
    pub(crate) test_run_cards: PersistTracked<BTreeMap<String, TestRunCardRef>>,
    /// Pending run waits (`test_run_waits.rs`). Persisted; a wait exists only
    /// while pending.
    pub(crate) test_run_waits: PersistTracked<Vec<TestRunWaitRecord>>,
    /// Authenticated origin facts, independent of transient destination waits.
    pub(crate) remote_test_run_facts: PersistTracked<BTreeMap<String, RemoteTestRunFacts>>,
    /// Durable delegation rows isolated during startup validation. See
    /// `quarantined_persisted_session_ids` for the preservation contract.
    pub(crate) quarantined_persisted_delegation_ids: BTreeSet<String>,
    /// Pending parent resumes waiting on one or more child delegation results.
    pub(crate) delegation_waits: PersistTracked<Vec<DelegationWaitRecord>>,
    /// Runtime-only mutation stamps for delegation rows. The background persist
    /// worker compares these with its watermark to upsert only delegation rows
    /// that changed since the last successful write.
    pub(crate) delegation_mutation_stamps: BTreeMap<String, u64>,
    /// Maintains bounded native-catalog child exclusion without status churn.
    pub(crate) delegation_catalog_children: Mutex<FederationCatalogChildren>,
    /// Runtime-only delegation tombstones drained by the persist thread and
    /// restored on write failure, mirroring session tombstone retry behavior.
    pub(crate) removed_delegation_ids: BTreeMap<String, u64>,
    /// Indexes currently running read-only delegation records by `delegations` index.
    pub(crate) running_read_only_delegations: BTreeSet<usize>,
    /// Process-local state of the cold finished-child transcript rule.
    pub(crate) cold_transcripts: ColdTranscriptState,
    /// Server-backed workspace documents keyed by workspace id.
    pub(crate) workspace_layouts: PersistTracked<BTreeMap<String, WorkspaceLayoutDocument>>,
    /// Monotonic counter used to stamp mutated session records. The persist
    /// thread uses this plus its own watermark to write only the sessions
    /// that changed since the last successful persist, so each
    /// `commit_locked` pays only for the sessions it actually touched
    /// rather than rewriting every session row. Starts at 0; advanced by
    /// [`StateInner::next_mutation_stamp`] on every `session_mut*` call.
    pub(crate) last_mutation_stamp: u64,
    /// Session ids removed from `sessions` since the last persist tick.
    /// Drained by the persist thread and applied as targeted `DELETE`
    /// statements so removed rows do not linger after the move to
    /// delta persistence.
    pub(crate) removed_session_ids: Vec<String>,
    /// Process-local: outstanding Claude work removed sessions left, kept so
    /// its interference outlives their records (`claude_outstanding_work.rs`).
    pub(crate) claude_orphaned_work: Vec<ClaudeOrphanedWork>,
}

impl StateInner {
    /// Creates a new instance.
    pub(crate) fn new() -> Self {
        Self {
            metadata_predecessor: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            metadata_watermarks: Arc::default(),
            state_projection_cache: Mutex::new(StateProjectionCache::default()),
            state_lock_epoch: next_state_lock_epoch(),
            federation: FederationState::default(),
            federation_durable: false,
            federation_durability_diagnostics: FederationDurabilityDiagnostics::new(
                false,
                "state_initial",
            ),
            federation_continuity_lost: false,
            federation_attestation: HashMap::new(),
            codex: CodexState::default(),
            engram_host_adapter: Arc::new(EngramHostAdapter::default()),
            engram_budget_clock: EngramBudgetClock::default(),
            #[cfg(test)]
            engram_budget_clock_selected: false,
            #[cfg(test)]
            engram_budget_clock_snapshots: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            test_engram_authority_ack_boundary: None,
            #[cfg(test)]
            test_delegation_child_admission_stall: None,
            engram_declared_project_ids: HashSet::new(),
            engram_declaration_checked_project_ids: HashSet::new(),
            engram_project_resets: EngramProjectResetFences::default(),
            preferences: AppPreferences::default(),
            settings_persist_dirty: false,
            remote_settings_persist_dirty: false,
            remote_delta_persist_dirty: false,
            revision: 0,
            next_session_number: 1,
            next_message_number: 1,
            projects: PersistTracked::default(),
            engram_retired_work_authority_grants: PersistTracked::default(),
            engram_work_source_roots: PersistTracked::default(),
            engram_named_root_journal: PersistTracked::default(),
            #[cfg(test)]
            test_engram_named_root_journal_limit: None,
            engram_work_naming_history: PersistTracked::default(),
            engram_named_root_release_audit: PersistTracked::default(),
            engram_source_sightings: PersistTracked::default(),
            engram_session_obligations: EngramSessionObligationLedger::empty(),
            engram_import_reconciliation: PersistTracked::default(),
            engram_root_read_cursor: BTreeMap::new(),
            engram_authority_read_cursor: BTreeMap::new(),
            engram_retained_run_reads: EngramRetainedRunReads::default(),
            engram_root_reclamation: EngramRootReclamationSchedule::default(),
            engram_source_root_generation: 0,
            pending_coordination_scope_deletions: PersistTracked::default(),
            pending_response_board_project_detachments: PersistTracked::default(),
            ignored_discovered_codex_thread_ids: PersistTracked::default(),
            remote_applied_revisions: HashMap::new(),
            remote_snapshot_applied_revisions: HashMap::new(),
            remote_session_transcript_applied_revisions: HashMap::new(),
            sessions: SessionSequence::default(),
            quarantined_persisted_session_ids: BTreeSet::new(),
            orchestrator_instances: PersistTracked::default(),
            delegations: Vec::new(),
            delegation_followup_admissions: HashMap::new(),
            acceptance_evaluation_submissions_in_flight: HashMap::new(),
            acceptance_evidence_recovery: HashMap::new(),
            review_packet_retention_failures: HashMap::new(),
            acceptance_source_captures_live: Arc::default(),
            engram_turn_basis_captures_live: Arc::default(),
            #[cfg(test)]
            test_engram_source_capture: None,
            #[cfg(test)]
            test_observation_source_owner: None,
            #[cfg(test)]
            test_declared_run_snapshot: None,
            #[cfg(test)]
            test_declared_run_clock: None,
            #[cfg(test)]
            test_declared_run_source: None,
            #[cfg(test)]
            test_native_source_owner_metadata: None,
            #[cfg(test)]
            test_acceptance_source_owner_capture: None,
            engram_source_root_validations_live: Arc::default(),
            test_runs: TestRunIndex::default(),
            test_run_registrations: TestRunRegistrationStore::default(),
            test_run_observations: TestRunObservationStore::default(),
            test_run_compacted: TestRunCompactedStore::default(),
            test_run_unassociated_receipts: HashMap::new(),
            test_run_record_pending: HashMap::new(),
            test_run_look_sequence: 0,
            test_run_scan_generation: 0,
            test_run_record_final_at_scan: HashMap::new(),
            test_run_record_bodies_built: HashSet::new(),
            test_run_record_looks_in_flight: HashMap::new(),
            test_run_record_look_published: HashMap::new(),
            test_run_unmirrored_settlements: BTreeSet::new(),
            test_run_cards_epoch: None,
            test_run_cards: PersistTracked::default(),
            test_run_waits: PersistTracked::default(),
            remote_test_run_facts: PersistTracked::default(),
            quarantined_persisted_delegation_ids: BTreeSet::new(),
            delegation_inventory: DelegationInventoryState::Ready,
            delegation_waits: PersistTracked::default(),
            delegation_mutation_stamps: BTreeMap::new(),
            delegation_catalog_children: Mutex::new(FederationCatalogChildren::default()),
            removed_delegation_ids: BTreeMap::new(),
            running_read_only_delegations: BTreeSet::new(),
            cold_transcripts: ColdTranscriptState::default(),
            workspace_layouts: PersistTracked::default(),
            last_mutation_stamp: 0,
            removed_session_ids: Vec::new(),
            claude_orphaned_work: Vec::new(),
        }
    }

    /// Returns whether the supplied remote snapshot revision is stale for this remote.
    pub(crate) fn should_skip_remote_applied_revision(
        &self,
        remote_id: &str,
        remote_revision: u64,
    ) -> bool {
        self.remote_applied_revisions
            .get(remote_id)
            .is_some_and(|latest_revision| *latest_revision >= remote_revision)
    }

    /// Returns whether the supplied remote state snapshot revision is stale for
    /// this remote. Same-revision snapshots are allowed after ordinary deltas so
    /// repair paths can materialize state after a sibling delta at that revision.
    pub(crate) fn should_skip_remote_applied_snapshot_revision(
        &self,
        remote_id: &str,
        remote_revision: u64,
    ) -> bool {
        self.remote_applied_revisions
            .get(remote_id)
            .is_some_and(|latest_revision| *latest_revision > remote_revision)
            || self
                .remote_snapshot_applied_revisions
                .get(remote_id)
                .is_some_and(|latest_revision| *latest_revision >= remote_revision)
    }

    /// Returns whether the supplied remote delta revision is stale for this remote.
    pub(crate) fn should_skip_remote_applied_delta_revision(
        &self,
        remote_id: &str,
        remote_revision: u64,
    ) -> bool {
        self.remote_applied_revisions
            .get(remote_id)
            .is_some_and(|latest_revision| *latest_revision > remote_revision)
            || self
                .remote_snapshot_applied_revisions
                .get(remote_id)
                .is_some_and(|latest_revision| *latest_revision > remote_revision)
    }

    /// Returns whether a session-scoped remote delta is stale for this remote
    /// session. This extends the broad remote delta rule with focused transcript
    /// hydration for one remote session only.
    pub(crate) fn should_skip_remote_session_applied_delta_revision(
        &self,
        remote_id: &str,
        remote_session_id: &str,
        remote_revision: u64,
    ) -> bool {
        self.should_skip_remote_applied_delta_revision(remote_id, remote_revision)
            || self
                .remote_session_transcript_applied_revision(remote_id, remote_session_id)
                .is_some_and(|latest_revision| latest_revision >= remote_revision)
    }

    pub(crate) fn remote_session_transcript_applied_revision(
        &self,
        remote_id: &str,
        remote_session_id: &str,
    ) -> Option<u64> {
        self.remote_session_transcript_applied_revisions
            .get(remote_id)
            .and_then(|sessions| sessions.get(remote_session_id))
            .copied()
    }

    /// Records the latest applied remote revision for a mirrored remote.
    pub(crate) fn note_remote_applied_revision(&mut self, remote_id: &str, remote_revision: u64) {
        self.remote_applied_revisions
            .entry(remote_id.to_owned())
            .and_modify(|latest_revision| {
                *latest_revision = (*latest_revision).max(remote_revision);
            })
            .or_insert(remote_revision);
    }

    /// Records that a broad full-state snapshot, not just a narrow delta or
    /// focused session response, has materialized this remote revision.
    pub(crate) fn note_remote_applied_snapshot_revision(
        &mut self,
        remote_id: &str,
        remote_revision: u64,
    ) {
        self.note_remote_applied_revision(remote_id, remote_revision);
        self.remote_snapshot_applied_revisions
            .entry(remote_id.to_owned())
            .and_modify(|latest_revision| {
                *latest_revision = (*latest_revision).max(remote_revision);
            })
            .or_insert(remote_revision);
    }

    /// Records that a focused bounded-tail response materialized the retained
    /// suffix for one remote session at this revision.
    pub(crate) fn note_remote_session_transcript_applied_revision(
        &mut self,
        remote_id: &str,
        remote_session_id: &str,
        remote_revision: u64,
    ) {
        self.remote_session_transcript_applied_revisions
            .entry(remote_id.to_owned())
            .or_default()
            .entry(remote_session_id.to_owned())
            .and_modify(|latest_revision| {
                *latest_revision = (*latest_revision).max(remote_revision);
            })
            .or_insert(remote_revision);
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct EngramMcpInstalledDescriptor {
    pub(crate) binary_path: String,
    pub(crate) home: String,
    /// Exact principal installed into the live agent process/thread and its
    /// Engram MCP child. Runtime-reported model synchronization must not
    /// silently change this frozen identity.
    pub(crate) actor_id: String,
    pub(crate) actor_context: Option<String>,
    pub(crate) store_key: Option<EngramAuthorityStoreKey>,
}

impl std::fmt::Debug for EngramMcpInstalledDescriptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngramMcpInstalledDescriptor")
            .field("binary_path", &self.binary_path)
            .field("home", &self.home)
            .field("actor_id", &self.actor_id)
            .field("actor_context", &self.actor_context)
            .field("store_key", &self.store_key)
            .finish()
    }
}

/// Represents a session record.
#[derive(Clone)]
pub(crate) struct SessionRecord {
    /// Explicit domain participation; never inferred from a name or local id.
    pub(crate) federation_mailbox_binding: Option<FederationMailboxBinding>,
    /// Routing only. Older receivers ignore this field and retain no grant.
    pub(crate) federation_automatic_mailbox_binding: Option<FederationMailboxBinding>,
    pub(crate) federation_retired_automatic_mailbox_bindings: Vec<FederationMailboxBinding>,
    pub(crate) federation_native_creation_id: String,
    /// Starts at zero when loaded; never persisted across server instances.
    pub(crate) body_sequence: SessionBodySequence,
    /// Process-local synchronization; rebuilt only from matching archive provenance.
    pub(crate) codex_delegation_release: Option<Arc<CodexDelegationRelease>>,
    /// Private durable ownership of an automatic archive, bound to its thread/home.
    pub(crate) codex_terminal_archive: Option<CodexTerminalArchive>,
    pub(crate) active_codex_approval_policy: Option<CodexApprovalPolicy>,
    pub(crate) active_codex_reasoning_effort: Option<CodexReasoningEffort>,
    pub(crate) active_codex_sandbox_mode: Option<CodexSandboxMode>,
    /// Durable once-per-interruption recovery; never an approval or grant.
    pub(crate) restart_recovery: Option<SessionRestartRecovery>,
    /// Process-local identity of the currently active turn. Incremented for
    /// every dispatch so asynchronous callbacks cannot mistake a successor
    /// turn on the same shared runtime for the turn that armed them.
    pub(crate) active_turn_generation: u64,
    /// Process-local exact human opening. It grants no Engram authority and
    /// goes inert when a successor generation starts. Queued provenance and
    /// an unknown handoff remain durable independently of this live owner.
    pub(crate) human_prompt_turn: Option<HumanPromptTurn>,
    /// Process-local coverage; never recovered from a cursor or durable audit.
    pub(crate) mailbox_continuations: MailboxContinuationCoverage,
    /// Exact durable mailbox boundary represented by the active turn, if any.
    /// Failure/Stop recovery takes this value before draining the next queued
    /// turn so it never requeues a successor's delivery.
    pub(crate) active_turn_mailbox_notification: Option<MailboxNotificationDelivery>,
    pub(crate) active_turn_start_message_count: Option<usize>,
    /// Process-local, non-reused ownership of watcher captures; rollback never restores it.
    pub(crate) file_change_tracking: Option<Arc<FileChangeTrackingLifetime>>,
    pub(crate) active_turn_file_changes: BTreeMap<String, WorkspaceFileChangeKind>,
    pub(crate) active_turn_file_change_grace_deadline: Option<std::time::Instant>,
    /// Process-local: a Claude segment no prompt owns is open
    /// (`claude_runtime_turns.rs`). While it is, nothing the recorder sees is
    /// reported to Engram. It closes with its segment's result or runtime.
    pub(crate) unmediated_claude_turn: Option<UnmediatedClaudeTurn>,
    /// Process-local: the turn generation the host adopted for a turn Claude
    /// Code started by itself. It keeps adopted execution separate from a
    /// host prompt and goes inert when a successor generation begins.
    pub(crate) adopted_claude_turn_generation: Option<u64>,
    /// Process-local: the Claude work that may still run and write without
    /// a later frame saying so (`claude_outstanding_work.rs`): subagent tool
    /// calls and background launches, by the runtime and turn that started
    /// them. It prevents checks from claiming an isolated execution while
    /// unrelated work may still write the same sources.
    pub(crate) claude_outstanding: ClaudeOutstandingWork,
    /// Process-local: what the recorder saw in turns no prompt owned, kept as
    /// unassigned and never credited, bounded and newest last.
    pub(crate) unassigned_claude_observations: VecDeque<UnassignedClaudeObservation>,
    pub(crate) agent_commands: Vec<AgentCommand>,
    pub(crate) codex_approval_policy: CodexApprovalPolicy,
    pub(crate) codex_reasoning_effort: CodexReasoningEffort,
    pub(crate) codex_sandbox_mode: CodexSandboxMode,
    /// The app-server profile whose Codex home holds this session's thread,
    /// fixed when the session attaches to an app-server before it has a
    /// thread. `None` for a thread created before profiles existed, which
    /// lives in the default home. See `shared_codex_profile.rs`.
    pub(crate) codex_thread_profile: Option<SharedCodexProfile>,
    pub(crate) external_session_id: Option<String>,
    pub(crate) pending_claude_approvals: HashMap<String, ClaudePendingApproval>,
    pub(crate) pending_claude_user_inputs: HashMap<String, ClaudePendingUserInput>,
    pub(crate) pending_codex_approvals: HashMap<String, CodexPendingApproval>,
    pub(crate) pending_codex_user_inputs: HashMap<String, CodexPendingUserInput>,
    pub(crate) pending_codex_mcp_elicitations: HashMap<String, CodexPendingMcpElicitation>,
    pub(crate) pending_codex_app_requests: HashMap<String, CodexPendingAppRequest>,
    pub(crate) pending_acp_approvals: HashMap<String, AcpPendingApproval>,
    /// Arrival order for ACP permission requests. The map above owns the
    /// routing payload; this queue makes the protocol's multiple-pending
    /// behavior deterministic and prevents later requests from overtaking the
    /// first unresolved approval.
    pub(crate) pending_acp_approval_order: VecDeque<String>,
    /// FIFO follow-up prompts collected while the runtime is busy.
    pub(crate) queued_prompts: VecDeque<QueuedPromptRecord>,
    /// Original peer messages represented by a coalesced queued prompt, keyed
    /// by that prompt's stable id. Ordinary queued prompts have no entry.
    pub(crate) queued_peer_messages: HashMap<String, Vec<PendingPrompt>>,
    /// Queued prompts that may not be promoted until their exact content is
    /// durable, keyed by prompt id (`queued_prompt_durability.rs`). Empty for
    /// every ordinary prompt.
    pub(crate) queued_prompt_durability_holds: BTreeMap<String, QueuedPromptDurabilityHold>,
    /// Delivery receipts of receipted prompts, keyed by prompt id
    /// (`prompt_delivery.rs`). Empty for every ordinary prompt.
    pub(crate) prompt_deliveries: BTreeMap<String, PromptDelivery>,
    /// Exact human choices beside the existing queue/receipt owners. A choice
    /// is provenance for a new decision, never proof of an earlier delivery.
    pub(crate) human_queue_choices: BTreeMap<String, Value>,
    /// Process-local observations only; no receipt or dispatch authority.
    pub(crate) provider_handoff_observations: ProviderHandoffObservations,
    /// Global transcript position represented by `session.messages[0]`.
    ///
    /// Persisted sessions load only a bounded recent tail into memory. New
    /// sessions and fully localized remote sessions start at zero. Message
    /// deltas translate local vec indices through this offset so browser
    /// cursors keep using stable whole-transcript positions.
    pub(crate) message_start_index: usize,
    pub(crate) message_positions: HashMap<String, usize>,
    /// Present only for proxy sessions mirrored from a remote TermAl backend.
    pub(crate) remote_id: Option<String>,
    pub(crate) remote_session_id: Option<String>,
    pub(crate) runtime: SessionRuntime,
    /// Process-local selection applied at provider start/resume, never persisted.
    pub(crate) project_environment: AppliedProjectEnvironment,
    #[cfg(windows)]
    pub(crate) retained_dedicated_owners: Vec<RetainedDedicatedOwner>,
    /// Descriptor actually installed into the currently attached local agent
    /// runtime/thread. This process-local capability record is deliberately
    /// omitted from persistence and wire snapshots; it exists so a later
    /// clear/disable/delete can revoke a superseded grant against the binary
    /// and home that received it, and so Engram control/context work keeps the
    /// exact actor identity installed into that runtime.
    pub(crate) engram_mcp_installed: Option<EngramMcpInstalledDescriptor>,
    pub(crate) runtime_reset_required: bool,
    /// A settings-only Codex MCP config change, consumed before the next turn.
    /// The current turn retains its live approval and interaction ownership.
    pub(crate) codex_mcp_config_refresh_pending: bool,
    /// A stale Engram MCP runtime whose process termination failed and whose
    /// handle is retained for retry. Unlike an ordinary settings reset, its
    /// natural exit must preserve the automatic-resume safety latch.
    pub(crate) engram_mcp_runtime_quarantined: bool,
    /// Persisted explicit-resume guard. User Stop and failed stop persistence
    /// both block mailbox/workflow auto-dispatch until a manual user turn
    /// resets the latch in `start_turn_on_record`.
    pub(crate) orchestrator_auto_dispatch_blocked: bool,
    /// Why the latch above is set, kept until the pause is lifted. Absent for
    /// a pause recorded before reasons existed.
    pub(crate) queue_pause_reason: Option<QueuePauseReason>,
    /// In memory: the typed kind the provider reported for the current turn's
    /// failure, read when that failure pauses the queue. A new turn clears it.
    pub(crate) turn_failure_kind: Option<TurnFailureKind>,
    pub(crate) provider_capacity_retry: Option<ProviderCapacityRetry>,
    pub(crate) codex_terminal_identity_unresolved: bool,
    pub(crate) runtime_stop_in_progress: bool,
    /// Token-scoped owner for the runtime callback fence. The generation
    /// prevents a stale Stop/revocation completion from releasing a newer
    /// owner's fence after the runtime has been replaced.
    pub(crate) runtime_stop_owner: Option<RuntimeStopOwner>,
    /// Monotonic generation of actual runtime Stop ownership.
    pub(crate) runtime_stop_generation: u64,
    /// In memory: advanced by every pause of the queue, from any source, so a
    /// Resume's continuation bound to the pause it crossed is ended by a later
    /// pause even when the queue reads paused again (prompt_delivery.rs).
    pub(crate) auto_dispatch_pause_epoch: u64,
    // In-memory ownership of an operator Stop. Cleanup can reassert the
    // queue latch without superseding a Human Send accepted after that Stop.
    pub(crate) operator_stop_generation: u64,
    /// An immediate Engram MCP revocation arrived while an ordinary Stop owned
    /// the runtime callback fence. Stop completion consumes this marker: a
    /// successful Stop has no runtime left to revoke, while a failed Stop
    /// transfers its fence directly to the revocation teardown.
    pub(crate) engram_mcp_revocation_pending: bool,
    /// Terminal callbacks deferred while `runtime_stop_in_progress` was true. Each
    /// callback keeps the active-turn generation it observed so replay cannot land
    /// on a successor turn that reused the same persistent runtime token. Replayed
    /// in arrival order on dedicated stop failure so the session doesn't get stuck
    /// in a stale Active state or reconstruct the wrong terminal sequence when
    /// completion/error and runtime-exit both land during the shutdown window.
    pub(crate) deferred_stop_callbacks: Vec<DeferredStopCallback>,
    /// Host-private Engram binding and open-turn state. It is persisted in the
    /// session row but never copied onto the wire-level `Session`.
    pub(crate) engram: EngramSessionState,
    pub(crate) hidden: bool,
    pub(crate) session: Session,
    /// Monotonic mutation stamp assigned by [`StateInner::next_mutation_stamp`]
    /// every time this record is handed out through one of the
    /// `session_mut*` helpers. Not persisted — stamps start at `0` on each
    /// process lifetime and the persist thread's watermark advances
    /// accordingly. A stamp strictly greater than the persist watermark
    /// means this record has in-memory changes that have not yet reached
    /// SQLite.
    pub(crate) mutation_stamp: u64,
    /// Process-local identity of this record's mutation-stamp sequence.
    /// Clones retain it; loading or creating a record starts a new sequence.
    pub(crate) persist_incarnation: Arc<()>,
    /// Last global mutation stamp at which the bounded composer history
    /// changed. The persist delta uses this independent watermark to keep the
    /// potentially large history out of hot session-metadata rewrites.
    pub(crate) prompt_history_mutation_stamp: u64,
}

/// Pre-promotion state captured before a queued prompt is promoted to an
/// active turn, so a failed persist commit can put the record back exactly
/// where it was instead of leaving a half-started turn in memory: head gone,
/// latch cleared, status Active, and nothing delivered to the runtime.
pub(crate) struct QueuePromotionSnapshot {
    pub(crate) work_binding: Option<EngramControlWorkBinding>,
    pub(crate) named_root: Option<EngramNamedRootState>,
    pub(crate) pending_naming_selection: Option<EngramPendingNamingSelection>,
    pub(crate) restart_recovery: Option<SessionRestartRecovery>,
    pub(crate) unadmitted_active_turn: Option<EngramRuntimeObservedTurn>,
    queue_order: Vec<String>,
    pub(crate) body_sequence: SessionBodySequence,
    pub(crate) active_turn_generation: u64,
    pub(crate) human_prompt_turn: Option<HumanPromptTurn>,
    pub(crate) status: SessionStatus,
    pub(crate) preview: String,
    pub(crate) live_activity: Option<SessionLiveActivity>,
    pub(crate) auto_dispatch_blocked: bool,
    pub(crate) queue_pause_reason: Option<QueuePauseReason>,
    pub(crate) operator_paused: bool,
    pub(crate) pause_epoch: u64,
    pub(crate) active_turn_mailbox_notification: Option<MailboxNotificationDelivery>,
    pub(crate) active_turn_start_message_count: Option<usize>,
    // Appending the prompt message also appends composer history and resyncs
    // the retained-transcript projections; the rollback must undo those too.
    pub(crate) prompt_history: Vec<String>,
    pub(crate) prompt_history_mutation_stamp: u64,
    pub(crate) message_count: u32,
    pub(crate) messages_loaded: bool,
    // Promotion prunes the promoted entry's durability hold with the queue;
    // a restored entry gets its hold (an acknowledged marker) back.
    pub(crate) queued_prompt_durability_holds: BTreeMap<String, QueuedPromptDurabilityHold>,
    // The promoted entry's delivery receipt names the turn it was promoted
    // under; a restored entry gets its receipt as it was before promotion.
    pub(crate) prompt_deliveries: BTreeMap<String, PromptDelivery>,
}

impl SessionRecord {
    pub(crate) fn mailbox_binding(&self) -> Option<&FederationMailboxBinding> {
        self.federation_mailbox_binding
            .as_ref()
            .or(self.federation_automatic_mailbox_binding.as_ref())
    }

    pub(crate) fn dedicated_cleanup_holds_admission(&self) -> bool {
        #[cfg(windows)]
        if self
            .runtime_stop_owner
            .as_ref()
            .is_some_and(|owner| owner.kind == RuntimeStopOwnerKind::DedicatedReset)
        {
            return true;
        }
        self.runtime.dedicated_cleanup_failure().is_some() || self.dedicated_predecessor_pending()
    }

    // Transcript observations may continue while exact cleanup remains retryable.
    pub(crate) fn runtime_projection_allowed(&self) -> bool {
        self.runtime.dedicated_cleanup_failure().is_none()
    }

    pub(crate) fn runtime_preview(&self, preview: String) -> String {
        if self.runtime_projection_allowed() && !self.codex_terminal_identity_unresolved {
            preview
        } else {
            self.session.preview.clone()
        }
    }

    pub(crate) fn clear_runtime(&mut self) {
        self.codex_terminal_identity_unresolved = false;
        self.codex_mcp_config_refresh_pending = false;
        #[cfg(windows)]
        self.retained_dedicated_owners
            .retain(|owner| !owner.tree.is_confirmed());
        self.runtime = SessionRuntime::None;
        self.engram_mcp_installed = None;
        // A runtime-started turn ends with its runtime.
        self.unmediated_claude_turn = None;
        // So does every Codex test carried until its runtime reports its end.
        EngramHost::runtime_cleared(self);
    }

    pub(crate) fn capture_queue_promotion_snapshot(&self) -> QueuePromotionSnapshot {
        QueuePromotionSnapshot {
            work_binding: self.engram.work_binding.clone(),
            named_root: self.engram.named_root.clone(),
            pending_naming_selection: self.engram.pending_naming_selection.clone(),
            restart_recovery: self.restart_recovery.clone(),
            unadmitted_active_turn: self.engram.unadmitted_active_turn.clone(),
            queue_order: self
                .queued_prompts
                .iter()
                .map(|queued| queued.pending_prompt.id.clone())
                .collect(),
            body_sequence: self.body_sequence.clone(),
            active_turn_generation: self.active_turn_generation,
            human_prompt_turn: self.human_prompt_turn.clone(),
            status: self.session.status,
            preview: self.session.preview.clone(),
            live_activity: self.session.live_activity.clone(),
            auto_dispatch_blocked: self.orchestrator_auto_dispatch_blocked,
            queue_pause_reason: self.queue_pause_reason.clone(),
            operator_paused: self.engram.operator_paused,
            pause_epoch: self.auto_dispatch_pause_epoch,
            active_turn_mailbox_notification: self.active_turn_mailbox_notification.clone(),
            active_turn_start_message_count: self.active_turn_start_message_count,
            prompt_history: self.session.prompt_history.clone(),
            prompt_history_mutation_stamp: self.prompt_history_mutation_stamp,
            message_count: self.session.message_count,
            messages_loaded: self.session.messages_loaded,
            queued_prompt_durability_holds: self.queued_prompt_durability_holds.clone(),
            prompt_deliveries: self.prompt_deliveries.clone(),
        }
    }

    /// Undoes a promotion whose persist commit failed: drops the prompt
    /// message the promotion appended, returns the queue head to the front,
    /// and restores the captured fields including the explicit-resume latch.
    /// All stored history stays passive. A runtime spawned for the
    /// attempt is left attached; an idle session with a live runtime is an
    /// ordinary state and the next promotion reuses it.
    pub(crate) fn restore_queue_promotion(
        &mut self,
        snapshot: QueuePromotionSnapshot,
        queued: QueuedPromptRecord,
        started_message_id: &str,
    ) {
        self.body_sequence = snapshot.body_sequence;
        if let Some(position) = self
            .session
            .messages
            .iter()
            .rposition(|message| message.id() == started_message_id)
            .filter(|_| queued.promoted_message_index.is_none())
        {
            self.session.messages.remove(position);
            self.message_positions = build_message_positions(&self.session.messages);
            // Mirror of insert_message_on_record: the retained-window
            // projections follow the transcript, and the composer history
            // the insertion appended goes back to its captured contents and
            // persistence stamp so the retry does not record it twice.
            sync_retained_transcript_metadata(self);
        }
        self.session.message_count = snapshot.message_count;
        self.session.messages_loaded = snapshot.messages_loaded;
        self.session.prompt_history = snapshot.prompt_history;
        self.prompt_history_mutation_stamp = snapshot.prompt_history_mutation_stamp;
        if let Some(current) = self
            .queued_prompts
            .iter_mut()
            .find(|current| current.pending_prompt.id == queued.pending_prompt.id)
        {
            *current = queued;
        } else {
            let position = snapshot
                .queue_order
                .iter()
                .take_while(|id| *id != &queued.pending_prompt.id)
                .filter(|id| {
                    self.queued_prompts
                        .iter()
                        .any(|entry| &entry.pending_prompt.id == *id)
                })
                .count();
            self.queued_prompts.insert(position, queued);
        }
        // Holds of entries still queued come back; sync prunes the rest.
        self.queued_prompt_durability_holds = snapshot.queued_prompt_durability_holds;
        self.prompt_deliveries = snapshot.prompt_deliveries;
        sync_pending_prompts(self);
        self.session.status = snapshot.status;
        self.restart_recovery = snapshot.restart_recovery;
        self.active_turn_generation = snapshot.active_turn_generation;
        self.engram.unadmitted_active_turn = snapshot.unadmitted_active_turn;
        self.engram.work_binding = snapshot.work_binding;
        self.engram.named_root = snapshot.named_root;
        self.engram.pending_naming_selection = snapshot.pending_naming_selection;
        self.human_prompt_turn = snapshot.human_prompt_turn;
        self.file_change_tracking = None;
        self.session.preview = snapshot.preview;
        self.session.live_activity = snapshot.live_activity;
        self.active_turn_mailbox_notification = snapshot.active_turn_mailbox_notification;
        self.active_turn_start_message_count = snapshot.active_turn_start_message_count;
        self.orchestrator_auto_dispatch_blocked = snapshot.auto_dispatch_blocked;
        self.session.queue_paused = snapshot.auto_dispatch_blocked;
        self.queue_pause_reason = snapshot.queue_pause_reason;
        self.engram.operator_paused = snapshot.operator_paused;
        self.auto_dispatch_pause_epoch = snapshot.pause_epoch;
    }

    /// Sets the explicit-resume latch and mirrors it onto the embedded
    /// session shape in the same step. The record flag is the authority;
    /// wire projections (`wire_session_from_record`, the metadata summary)
    /// re-derive `queue_paused` from it on every build, so the mirror here
    /// only keeps the in-record copy honest for readers that inspect
    /// `record.session` directly. Production code must flip the latch only
    /// through this helper.
    pub(crate) fn set_auto_dispatch_blocked(&mut self, blocked: bool) {
        self.orchestrator_auto_dispatch_blocked = blocked;
        self.session.queue_paused = blocked;
        if blocked {
            self.auto_dispatch_pause_epoch = self.auto_dispatch_pause_epoch.wrapping_add(1);
        }
        if !blocked {
            // Lifting the pause lifts an operator's pause with it, and the
            // source-observation hold that only an explicit Resume lifts.
            self.engram.operator_paused = false;
            self.queue_pause_reason = None;
        }
    }

    /// An operator's Stop pauses the queue, records `stop_requested` as the
    /// reason, and marks the pause as the operator's.
    pub(crate) fn pause_queue_for_operator_stop(&mut self) {
        self.pause_auto_dispatch(QueuePauseReason::now(QueuePauseCause::StopRequested));
        self.engram.operator_paused = true;
        self.operator_stop_generation = self.operator_stop_generation.wrapping_add(1);
    }

    /// Pauses the queue and records why, replacing any earlier reason, so a
    /// coordinator or the sender of a held wake can see the cause without
    /// reading this session. Production pauses go through here.
    pub(crate) fn pause_auto_dispatch(&mut self, reason: QueuePauseReason) {
        self.set_auto_dispatch_blocked(true);
        self.queue_pause_reason = Some(reason);
    }

    /// Pauses for a failed turn, with the typed kind its provider reported for
    /// this turn, if any, and the failure's redacted one-line summary. A
    /// provider-capacity pause that its automatic retry owns keeps that cause,
    /// so the reason does not claim the queue waits for someone to look.
    /// `summary` is the failure's `queue_pause_summary`, computed before the
    /// state lock was taken.
    pub(crate) fn pause_auto_dispatch_for_turn(
        &mut self,
        cause: QueuePauseCause,
        summary: Option<String>,
    ) {
        let kind = self.turn_failure_kind.take();
        let capacity_owns_pause = self.provider_capacity_retry.is_some()
            && self
                .queue_pause_reason
                .as_ref()
                .is_some_and(|reason| reason.cause == QueuePauseCause::ProviderCapacity);
        let cause = if capacity_owns_pause {
            QueuePauseCause::ProviderCapacity
        } else {
            cause
        };
        self.pause_auto_dispatch(QueuePauseReason::turn(cause, kind, summary));
    }

    pub(crate) fn clear_runtime_reset(&mut self) {
        self.runtime_reset_required = false;
        self.engram_mcp_runtime_quarantined = false;
    }

    pub(crate) fn claim_runtime_stop(
        &mut self,
        kind: RuntimeStopOwnerKind,
        token: RuntimeToken,
    ) -> u64 {
        self.runtime_stop_generation = self.runtime_stop_generation.wrapping_add(1).max(1);
        let generation = self.runtime_stop_generation;
        self.runtime_stop_in_progress = true;
        self.runtime_stop_owner = Some(RuntimeStopOwner {
            kind,
            token: Some(token),
            generation,
        });
        generation
    }

    /// Claims stop ownership for a session whose runtime handle has already
    /// disappeared. The generation fences the off-lock cleanup without
    /// fabricating an agent-specific runtime token.
    pub(crate) fn claim_missing_runtime_stop(&mut self, kind: RuntimeStopOwnerKind) -> u64 {
        self.runtime_stop_generation = self.runtime_stop_generation.wrapping_add(1).max(1);
        let generation = self.runtime_stop_generation;
        self.runtime_stop_in_progress = true;
        self.runtime_stop_owner = Some(RuntimeStopOwner {
            kind,
            token: None,
            generation,
        });
        generation
    }

    pub(crate) fn clear_runtime_stop(&mut self) {
        self.runtime_stop_in_progress = false;
        self.runtime_stop_owner = None;
        self.engram_mcp_revocation_pending = false;
    }

    pub(crate) fn runtime_stop_is_owned_by(
        &self,
        kind: RuntimeStopOwnerKind,
        token: &RuntimeToken,
        generation: u64,
    ) -> bool {
        self.runtime_stop_owner.as_ref().is_some_and(|owner| {
            owner.kind == kind
                && owner.token.as_ref() == Some(token)
                && owner.generation == generation
        })
    }

    pub(crate) fn missing_runtime_stop_is_owned_by(
        &self,
        kind: RuntimeStopOwnerKind,
        generation: u64,
    ) -> bool {
        self.runtime_stop_owner.as_ref().is_some_and(|owner| {
            owner.kind == kind && owner.token.is_none() && owner.generation == generation
        })
    }

    /// Returns the paired remote identity when this record is a valid proxy.
    pub(crate) fn remote_proxy_identity(&self) -> Result<Option<(&str, &str)>> {
        validate_remote_proxy_identity(self.remote_id.as_deref(), self.remote_session_id.as_deref())
    }

    /// Returns whether remote proxy.
    pub(crate) fn is_remote_proxy(&self) -> bool {
        self.remote_proxy_identity()
            .is_ok_and(|identity| identity.is_some())
    }

    /// Returns whether this record has the canonical local identity.
    ///
    /// An invalid partial remote identity is deliberately neither local nor a
    /// remote proxy, so local-only paths fail closed instead of acting on it.
    pub(crate) fn is_local_session(&self) -> bool {
        self.remote_proxy_identity()
            .is_ok_and(|identity| identity.is_none())
    }
}

/// Handles Codex approval policy from JSON value.
pub(crate) fn codex_approval_policy_from_json_value(value: &Value) -> Option<CodexApprovalPolicy> {
    match value {
        Value::String(raw) => match raw.as_str() {
            "untrusted" => Some(CodexApprovalPolicy::Untrusted),
            "on-failure" => Some(CodexApprovalPolicy::OnFailure),
            "on-request" => Some(CodexApprovalPolicy::OnRequest),
            "auto-approve" => Some(CodexApprovalPolicy::AutoApprove),
            "never" => Some(CodexApprovalPolicy::Never),
            _ => None,
        },
        _ => None,
    }
}

/// Handles Codex reasoning effort from JSON value.
pub(crate) fn codex_reasoning_effort_from_json_value(
    value: &Value,
) -> Option<CodexReasoningEffort> {
    match value {
        Value::String(raw) => match raw.as_str() {
            "none" => Some(CodexReasoningEffort::None),
            "minimal" => Some(CodexReasoningEffort::Minimal),
            "low" => Some(CodexReasoningEffort::Low),
            "medium" => Some(CodexReasoningEffort::Medium),
            "high" => Some(CodexReasoningEffort::High),
            "xhigh" => Some(CodexReasoningEffort::XHigh),
            "max" => Some(CodexReasoningEffort::Max),
            "ultra" => Some(CodexReasoningEffort::Ultra),
            _ => None,
        },
        _ => None,
    }
}

/// Handles Codex sandbox mode from JSON value.
pub(crate) fn codex_sandbox_mode_from_json_value(value: &Value) -> Option<CodexSandboxMode> {
    match value {
        Value::String(raw) => match raw.as_str() {
            "danger-full-access" => Some(CodexSandboxMode::DangerFullAccess),
            "read-only" => Some(CodexSandboxMode::ReadOnly),
            "workspace-write" => Some(CodexSandboxMode::WorkspaceWrite),
            _ => None,
        },
        Value::Object(_) => match value.get("type").and_then(Value::as_str) {
            Some("dangerFullAccess") => Some(CodexSandboxMode::DangerFullAccess),
            Some("readOnly") => Some(CodexSandboxMode::ReadOnly),
            Some("workspaceWrite") => Some(CodexSandboxMode::WorkspaceWrite),
            _ => None,
        },
        _ => None,
    }
}

/// Returns the default forked Codex session name.
pub(crate) fn default_forked_codex_session_name(
    current_name: &str,
    thread_name: Option<&str>,
) -> String {
    let trimmed_thread_name = thread_name.map(str::trim).filter(|value| !value.is_empty());
    let trimmed_current_name = current_name.trim();
    let base = trimmed_thread_name.unwrap_or(trimmed_current_name);
    format!("{base} Fork")
}

/// Resolves forked Codex working directory.
pub(crate) fn resolve_forked_codex_workdir(
    requested_workdir: Option<&str>,
    fallback_workdir: &str,
    project_id: Option<&str>,
    state: &AppState,
) -> Result<String, ApiError> {
    let Some(requested_workdir) = requested_workdir
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(fallback_workdir.to_owned());
    };

    let project_id = match project_id {
        Some(project_id) => project_id,
        None => return Ok(requested_workdir.to_owned()),
    };
    let project_root = resolve_project_root_path_by_id(state, project_id)?;
    if path_contains(
        project_root.to_string_lossy().as_ref(),
        FsPath::new(requested_workdir),
    ) {
        Ok(requested_workdir.to_owned())
    } else {
        Ok(fallback_workdir.to_owned())
    }
}
