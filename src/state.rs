/*
State and persistence core
                 +------------------------+
REST / runtimes ->| AppState              |-> state_events
remote bridge  -> | - coordination shell  |-> delta_events
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

/// Wake signal sent to the background persist thread.
///
/// The persist thread owns an `Arc<StateMutex<StateInner>>` and collects
/// the diff itself on each tick — it captures a lightweight selection under
/// the global mutex, then clones each selected session/delegation in a separate
/// acquisition before writing to SQLite via `persist_delta_via_cache` (see
/// `persist.rs`).
/// `PersistRequest` therefore carries only the wake signal; the full
/// `PersistedState` snapshot that earlier versions cloned under the
/// state mutex is no longer needed.
enum PersistRequest {
    /// Incremental persist: the thread looks up the current
    /// `last_mutation_stamp` and writes only the sessions that
    /// advanced past the thread's own watermark.
    Delta,
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
const STATE_MUTEX_WARN_AFTER: Duration = Duration::from_millis(250);

const STATE_MUTEX_DIAGNOSTIC_QUEUE_CAPACITY: usize = 64;

#[derive(Clone, Debug)]
enum StateMutexDiagnostic {
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

type StateMutexDiagnosticReporter = Arc<dyn Fn(StateMutexDiagnostic) + Send + Sync>;

#[cfg(not(test))]
fn state_mutex_diagnostic_sender() -> &'static mpsc::SyncSender<StateMutexDiagnostic> {
    static SENDER: LazyLock<mpsc::SyncSender<StateMutexDiagnostic>> = LazyLock::new(|| {
        let (sender, receiver) = mpsc::sync_channel(STATE_MUTEX_DIAGNOSTIC_QUEUE_CAPACITY);
        if let Err(err) = std::thread::Builder::new()
            .name("termal-state-lock-diagnostics".to_owned())
            .spawn(move || {
                while let Ok(diagnostic) = receiver.recv() {
                    match diagnostic {
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

fn try_queue_state_mutex_diagnostic(
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

struct StateMutex<T> {
    inner: Mutex<T>,
    diagnostic_reporter: StateMutexDiagnosticReporter,
    warn_after: Duration,
    /// Test-only owner tracking distinguishes a callback that re-enters this
    /// exact mutex on the same thread from harmless contention by a fixture's
    /// background thread. That makes lock-scope assertions deterministic.
    #[cfg(test)]
    owner_thread: Mutex<Option<std::thread::ThreadId>>,
}

impl<T> StateMutex<T> {
    fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
            diagnostic_reporter: default_state_mutex_diagnostic_reporter(),
            warn_after: STATE_MUTEX_WARN_AFTER,
            #[cfg(test)]
            owner_thread: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn new_with_diagnostic_reporter(
        value: T,
        warn_after: Duration,
        diagnostic_reporter: StateMutexDiagnosticReporter,
    ) -> Self {
        Self {
            inner: Mutex::new(value),
            diagnostic_reporter,
            warn_after,
            owner_thread: Mutex::new(None),
        }
    }

    #[track_caller]
    fn lock(&self) -> std::sync::LockResult<StateMutexGuard<'_, T>> {
        let requested_at = std::time::Instant::now();
        let caller = std::panic::Location::caller();
        match self.inner.lock() {
            Ok(guard) => Ok(StateMutexGuard::new(
                guard,
                requested_at,
                caller,
                self.warn_after,
                Arc::clone(&self.diagnostic_reporter),
                #[cfg(test)]
                &self.owner_thread,
            )),
            Err(err) => Err(std::sync::PoisonError::new(StateMutexGuard::new(
                err.into_inner(),
                requested_at,
                caller,
                self.warn_after,
                Arc::clone(&self.diagnostic_reporter),
                #[cfg(test)]
                &self.owner_thread,
            ))),
        }
    }

    #[cfg(test)]
    fn is_not_held_by_current_thread_for_test(&self) -> bool {
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

struct StateMutexGuard<'a, T> {
    guard: Option<std::sync::MutexGuard<'a, T>>,
    acquired_at: std::time::Instant,
    caller: &'static std::panic::Location<'static>,
    diagnostic_reporter: StateMutexDiagnosticReporter,
    waited: Duration,
    warn_after: Duration,
    #[cfg(test)]
    owner_thread: &'a Mutex<Option<std::thread::ThreadId>>,
}

impl<'a, T> StateMutexGuard<'a, T> {
    fn new(
        guard: std::sync::MutexGuard<'a, T>,
        requested_at: std::time::Instant,
        caller: &'static std::panic::Location<'static>,
        warn_after: Duration,
        diagnostic_reporter: StateMutexDiagnosticReporter,
        #[cfg(test)] owner_thread: &'a Mutex<Option<std::thread::ThreadId>>,
    ) -> Self {
        let acquired_at = std::time::Instant::now();
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
            waited,
            warn_after,
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
        drop(self.guard.take());
        if held >= self.warn_after {
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

enum StateBroadcastWork {
    Snapshot(StateResponse),
    DeltaPayload(String),
}

const STATE_BROADCAST_MAILBOX_CAPACITY: usize = 256;

#[derive(Default)]
struct StateBroadcastMailbox {
    pending: Mutex<VecDeque<StateBroadcastWork>>,
    work_available: Condvar,
}

impl StateBroadcastMailbox {
    fn publish_snapshot(&self, snapshot: StateResponse) {
        let mut pending = self
            .pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned");
        if let Some(StateBroadcastWork::Snapshot(existing)) = pending.back_mut() {
            *existing = snapshot;
        } else {
            if pending.len() >= STATE_BROADCAST_MAILBOX_CAPACITY {
                pending.pop_front();
            }
            pending.push_back(StateBroadcastWork::Snapshot(snapshot));
        }
        self.work_available.notify_one();
    }

    fn publish_delta_payload(&self, payload: String) {
        let mut pending = self
            .pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned");
        if pending.len() >= STATE_BROADCAST_MAILBOX_CAPACITY {
            pending.pop_front();
        }
        pending.push_back(StateBroadcastWork::DeltaPayload(payload));
        self.work_available.notify_one();
    }

    fn recv_next(&self) -> StateBroadcastWork {
        let mut pending = self
            .pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned");
        loop {
            if let Some(work) = pending.pop_front() {
                return work;
            }
            pending = self
                .work_available
                .wait(pending)
                .expect("state broadcast mailbox mutex poisoned");
        }
    }

    #[cfg(test)]
    fn take_pending_for_test(&self) -> Vec<StateBroadcastWork> {
        let drained = self
            .pending
            .lock()
            .expect("state broadcast mailbox mutex poisoned")
            .drain(..)
            .collect();
        drained
    }
}

fn forward_state_broadcast_work(
    work: StateBroadcastWork,
    state_events: &broadcast::Sender<String>,
    delta_events: &broadcast::Sender<String>,
) {
    match work {
        StateBroadcastWork::Snapshot(snapshot) => match serde_json::to_string(&snapshot) {
            Ok(payload) => {
                let _ = state_events.send(payload);
            }
            Err(err) => {
                eprintln!(
                    "warning: failed to serialize SSE state snapshot at revision {}: {err}",
                    snapshot.revision,
                );
            }
        },
        StateBroadcastWork::DeltaPayload(payload) => {
            let _ = delta_events.send(payload);
        }
    }
}

#[cfg(test)]
mod state_broadcast_mailbox_tests {
    use super::*;

    fn snapshot(revision: u64) -> StateResponse {
        StateResponse {
            revision,
            server_instance_id: "test-instance".to_owned(),
            codex: CodexState::default(),
            agent_readiness: Vec::new(),
            preferences: AppPreferences::default(),
            projects: Vec::new(),
            orchestrators: Vec::new(),
            workspaces: Vec::new(),
            sessions: Vec::new(),
            delegations: Vec::new(),
            delegation_waits: Vec::new(),
            pending_engram_mcp_revocation_session_ids: Vec::new(),
        }
    }

    #[test]
    fn state_broadcast_mailbox_keeps_only_latest_pending_snapshot() {
        let mailbox = StateBroadcastMailbox::default();

        mailbox.publish_snapshot(snapshot(1));
        mailbox.publish_snapshot(snapshot(2));
        mailbox.publish_snapshot(snapshot(3));

        let pending = mailbox.take_pending_for_test();
        assert_eq!(pending.len(), 1);
        match &pending[0] {
            StateBroadcastWork::Snapshot(latest) => assert_eq!(latest.revision, 3),
            StateBroadcastWork::DeltaPayload(_) => panic!("expected latest snapshot"),
        }
        assert!(mailbox.take_pending_for_test().is_empty());
    }

    #[test]
    fn state_broadcast_mailbox_preserves_state_before_following_delta() {
        let mailbox = StateBroadcastMailbox::default();

        mailbox.publish_snapshot(snapshot(1));
        mailbox.publish_snapshot(snapshot(2));
        mailbox.publish_delta_payload("delta-3".to_owned());
        mailbox.publish_snapshot(snapshot(4));
        mailbox.publish_snapshot(snapshot(5));

        let pending = mailbox.take_pending_for_test();
        assert_eq!(pending.len(), 3);
        match &pending[0] {
            StateBroadcastWork::Snapshot(value) => assert_eq!(value.revision, 2),
            StateBroadcastWork::DeltaPayload(_) => panic!("expected coalesced snapshot first"),
        }
        match &pending[1] {
            StateBroadcastWork::DeltaPayload(value) => assert_eq!(value, "delta-3"),
            StateBroadcastWork::Snapshot(_) => panic!("expected delta second"),
        }
        match &pending[2] {
            StateBroadcastWork::Snapshot(value) => assert_eq!(value.revision, 5),
            StateBroadcastWork::DeltaPayload(_) => panic!("expected coalesced snapshot third"),
        }
    }

    #[test]
    fn state_broadcast_mailbox_coalesces_latest_snapshot_when_full() {
        let mailbox = StateBroadcastMailbox::default();

        for index in 1..STATE_BROADCAST_MAILBOX_CAPACITY {
            mailbox.publish_delta_payload(format!("delta-{index}"));
        }
        mailbox.publish_snapshot(snapshot(1));
        mailbox.publish_snapshot(snapshot(2));

        let pending = mailbox.take_pending_for_test();
        assert_eq!(pending.len(), STATE_BROADCAST_MAILBOX_CAPACITY);
        match pending.last() {
            Some(StateBroadcastWork::Snapshot(latest)) => assert_eq!(latest.revision, 2),
            _ => panic!("expected latest snapshot to replace the queued snapshot"),
        }
    }

    #[test]
    fn state_broadcast_mailbox_drops_oldest_delta_when_full() {
        let mailbox = Arc::new(StateBroadcastMailbox::default());
        for index in 0..STATE_BROADCAST_MAILBOX_CAPACITY {
            mailbox.publish_delta_payload(format!("delta-{index}"));
        }

        let (published_tx, published_rx) = std::sync::mpsc::channel();
        let publisher_mailbox = mailbox.clone();
        let publisher = std::thread::spawn(move || {
            publisher_mailbox.publish_delta_payload("delta-over-capacity".to_owned());
            published_tx
                .send(())
                .expect("completion signal should send");
        });

        published_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("publisher should not wait while the mailbox is full");
        publisher.join().expect("publisher thread should not panic");

        let pending = mailbox.take_pending_for_test();
        assert_eq!(pending.len(), STATE_BROADCAST_MAILBOX_CAPACITY);
        match pending.first() {
            Some(StateBroadcastWork::DeltaPayload(value)) => assert_eq!(value, "delta-1"),
            _ => panic!("expected oldest retained delta first"),
        }
        match pending.last() {
            Some(StateBroadcastWork::DeltaPayload(value)) => {
                assert_eq!(value, "delta-over-capacity")
            }
            _ => panic!("expected over-capacity delta to enqueue immediately"),
        }
    }

    #[test]
    fn state_broadcast_mailbox_drops_oldest_delta_for_snapshot_when_full() {
        let mailbox = StateBroadcastMailbox::default();
        for index in 0..STATE_BROADCAST_MAILBOX_CAPACITY {
            mailbox.publish_delta_payload(format!("delta-{index}"));
        }
        mailbox.publish_snapshot(snapshot(99));

        let pending = mailbox.take_pending_for_test();
        assert_eq!(pending.len(), STATE_BROADCAST_MAILBOX_CAPACITY);
        match pending.first() {
            Some(StateBroadcastWork::DeltaPayload(value)) => assert_eq!(value, "delta-1"),
            _ => panic!("expected oldest retained delta first"),
        }
        match pending.last() {
            Some(StateBroadcastWork::Snapshot(latest)) => assert_eq!(latest.revision, 99),
            _ => panic!("expected snapshot to enqueue immediately"),
        }
    }

    #[test]
    fn state_broadcast_mailbox_drops_oldest_snapshot_when_delta_arrives_full() {
        let mailbox = StateBroadcastMailbox::default();
        mailbox.publish_snapshot(snapshot(7));
        for index in 0..STATE_BROADCAST_MAILBOX_CAPACITY - 1 {
            mailbox.publish_delta_payload(format!("delta-{index}"));
        }
        mailbox.publish_delta_payload("delta-over-capacity".to_owned());

        let pending = mailbox.take_pending_for_test();
        assert_eq!(pending.len(), STATE_BROADCAST_MAILBOX_CAPACITY);
        match pending.first() {
            Some(StateBroadcastWork::DeltaPayload(value)) => assert_eq!(value, "delta-0"),
            _ => panic!("expected first retained item to be the first delta after dropped snapshot"),
        }
        match pending.last() {
            Some(StateBroadcastWork::DeltaPayload(value)) => {
                assert_eq!(value, "delta-over-capacity")
            }
            _ => panic!("expected over-capacity delta to enqueue immediately"),
        }
        assert!(
            pending
                .iter()
                .all(|work| matches!(work, StateBroadcastWork::DeltaPayload(_))),
            "snapshot head should be dropped as the oldest pending work"
        );
    }
}

const REMOTE_DELTA_REPLAY_CACHE_LIMIT: usize = 2048;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RemoteDeltaReplayKey {
    remote_id: String,
    /// Identifies the connection/config publication that owned this event.
    /// The same remote id, revision, and payload from a replacement endpoint
    /// must never be suppressed by bookkeeping left by its predecessor.
    authority_generation: u64,
    revision: u64,
    payload: RemoteDeltaReplayPayload,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RemoteDeltaHydrationKey {
    remote_id: String,
    remote_session_id: String,
    authority_generation: u64,
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
enum RemoteDeltaReplayPayload {
    SessionCreated {
        session_id: String,
        message_count: u32,
        session_fingerprint: String,
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
        session_mutation_stamp: Option<u64>,
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
    },
    TextReplace {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        text_fingerprint: String,
        preview_fingerprint: Option<String>,
        session_mutation_stamp: Option<u64>,
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
    },
    ParallelAgentsUpdate {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        agents_fingerprint: String,
        preview_fingerprint: String,
        session_mutation_stamp: Option<u64>,
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
        session_fingerprints: Vec<String>,
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
struct RemoteDeltaReplayCache {
    keys: HashSet<RemoteDeltaReplayKey>,
    order: VecDeque<RemoteDeltaReplayKey>,
}

impl RemoteDeltaReplayCache {
    fn contains(&self, key: &RemoteDeltaReplayKey) -> bool {
        self.keys.contains(key)
    }

    fn insert(&mut self, key: RemoteDeltaReplayKey) {
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

    fn remove_remote(&mut self, remote_id: &str) -> bool {
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
struct PersistDeltaPlan {
    metadata: PersistedState,
    changed_sessions: Vec<PersistSessionCandidate>,
    removed_session_ids: Vec<String>,
    changed_delegations: Vec<PersistDelegationCandidate>,
    removed_delegation_ids: Vec<String>,
    drained_delegation_tombstones: BTreeMap<String, u64>,
    drained_explicit_tombstones: Vec<String>,
    watermark: u64,
}

#[cfg_attr(test, allow(dead_code))]
struct PersistSessionCandidate {
    session_id: String,
    mutation_stamp: u64,
    persist_prompt_history: bool,
}

#[cfg_attr(test, allow(dead_code))]
struct PersistDelegationCandidate {
    delegation_id: String,
    mutation_stamp: u64,
}

enum PersistCandidateMaterialization<T> {
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
struct PersistDelta {
    metadata: PersistedState,
    changed_sessions: Vec<PersistedSessionRecord>,
    removed_session_ids: Vec<String>,
    changed_delegations: Option<Vec<DelegationRecord>>,
    removed_delegation_ids: Vec<String>,
    deferred_session_ids: Vec<String>,
    deferred_prompt_history_session_ids: Vec<String>,
    deferred_delegation_ids: Vec<String>,
    drained_delegation_tombstones: BTreeMap<String, u64>,
    drained_explicit_tombstones: Vec<String>,
    watermark: u64,
}

#[derive(Clone)]
struct AppState {
    /// Per-process UUID generated at `AppState::new_with_paths` boot.
    /// Carried on every `StateResponse` and `HealthResponse` so clients
    /// can distinguish "revision decreased because the server just
    /// restarted" from "revision decreased because this response is
    /// stale". The frontend's `shouldAdoptSnapshotRevision` uses a
    /// mismatch between this id and its `lastSeenServerInstanceIdRef`
    /// as the signal to accept a revision downgrade.
    server_instance_id: String,
    default_workdir: String,
    /// Local HTTP origin for agent-facing bridge processes that need to call
    /// TermAl's own API. Set after the server socket is bound; falls back to
    /// TERMAL_BASE_URL/TERMAL_PORT for tests and non-server modes.
    local_http_base_url: Arc<Mutex<Option<String>>>,
    persistence_path: Arc<PathBuf>,
    /// Durable neutral coordination mailboxes use their own long-lived SQLite
    /// connection. This deliberately bypasses the asynchronous state persist
    /// worker: append/fetch/ack must remain available during and after worker
    /// shutdown, and committed messages must be visible before any receiver
    /// wake-up is attempted.
    mailbox_store: Arc<MailboxStore>,
    /// Level-triggered sibling of the mailbox store for versioned,
    /// per-repository coordination facts. Mailbox and board each keep one
    /// long-lived connection to coordination.sqlite and share that file's FIFO
    /// writer admission, isolated from termal.sqlite session/transcript writes.
    /// Board writes never wake a session — activation stays mailbox-only.
    coordination_board_store: Arc<CoordinationBoardStore>,
    orchestrator_templates_path: Arc<PathBuf>,
    /// Must not be held at the same time as `self.inner`; template file I/O happens
    /// outside the main state mutex so we never invert lock ordering.
    orchestrator_templates_lock: Arc<Mutex<()>>,
    /// Must not be held at the same time as `self.inner`; review file I/O stays
    /// outside the main state mutex so disk writes do not stall unrelated state work.
    review_documents_lock: Arc<Mutex<()>>,
    state_events: broadcast::Sender<String>,
    delta_events: broadcast::Sender<String>,
    file_events: broadcast::Sender<String>,
    #[cfg_attr(test, allow(dead_code))]
    file_events_revision: Arc<AtomicU64>,
    /// Background persistence channel. `persist_internal_locked` sends a
    /// pre-cloned `PersistedState` snapshot through this channel; a
    /// dedicated thread serializes it to JSON and writes the file so the
    /// state mutex is never held during I/O.
    persist_tx: mpsc::Sender<PersistRequest>,
    /// Handle to the background persist thread. Wrapped in
    /// `Arc<Mutex<Option<_>>>` so that:
    ///   - `AppState` stays `Clone` (the handle is shared, not duplicated),
    ///   - exactly one shutdown caller can `take()` the handle and join,
    ///   - subsequent shutdown calls block behind the join owner and then
    ///     become a safe no-op.
    /// Populated by `AppState::new_with_paths` after spawning the thread.
    /// `None` for test-only constructors that don't spawn the thread —
    /// `shutdown_persist_blocking` then has nothing to wait on.
    persist_thread_handle: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
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
    persist_worker_alive: Arc<std::sync::atomic::AtomicBool>,
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
    shutdown_signal_tx: Arc<tokio::sync::watch::Sender<bool>>,
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
    state_broadcast_mailbox: Option<Arc<StateBroadcastMailbox>>,
    /// Owns the optional in-process Telegram relay runtime for this app
    /// instance. Kept outside `StateInner` because it is live process state
    /// (thread handle, shutdown flag, status), not durable app data.
    telegram_relay_runtime: Arc<Mutex<TelegramRelayRuntime>>,
    /// Lazily created shared Codex app-server reused across Codex sessions.
    shared_codex_runtime: Arc<Mutex<Option<SharedCodexRuntime>>>,
    /// Runtime ids whose exit cascade has already been claimed. Several
    /// worker threads may report one shared-process death; only the first may
    /// terminalize sessions or rebind Engram.
    shared_codex_exit_claims: Arc<Mutex<HashSet<String>>>,
    /// Whether this state may launch real local agent subprocesses. Production
    /// states enable spawning; lightweight test states disable it so ordinary
    /// state-transition tests cannot accidentally retain real agent runtimes.
    agent_runtime_spawning_enabled: bool,
    #[cfg(test)]
    test_acp_runtime_overrides: Arc<Mutex<Vec<TestAcpRuntimeOverride>>>,
    #[cfg(test)]
    test_agent_setup_failures: Arc<Mutex<Vec<(Agent, String)>>>,
    /// Cached app-level agent readiness lives outside `self.inner` so full
    /// snapshots can clone the latest value without filesystem work under the
    /// main state mutex.
    agent_readiness_cache: Arc<RwLock<AgentReadinessCache>>,
    /// Serializes cache refreshes so concurrent snapshot requests do not all
    /// repeat the same readiness filesystem probes.
    agent_readiness_refresh_lock: Arc<Mutex<()>>,
    /// Owns SSH-backed remote connections and their event bridges.
    remote_registry: Arc<RemoteRegistry>,
    /// Tracks the newest `_sseFallback` revision already recovered per remote
    /// so duplicate or older fallback events do not trigger redundant
    /// blocking `/api/state` fetches.
    remote_sse_fallback_resynced_revision: Arc<Mutex<HashMap<String, u64>>>,
    /// Exact remote-delta payload identities for every successfully applied
    /// inbound delta. Suppresses same-revision payload-identical replays from a
    /// misbehaving remote/SSE retry; sibling same-revision deltas with different
    /// payloads still apply. Per-remote entries are cleared on event-stream
    /// continuity loss.
    remote_delta_replay_cache: Arc<Mutex<RemoteDeltaReplayCache>>,
    /// Remote session transcript hydrations currently being fetched because a
    /// delta reached an unloaded proxy session. The key includes the authority
    /// generation so endpoint B is never blocked by endpoint A's retired
    /// request for the same remote/session ids. Same-generation duplicates skip
    /// the narrow unloaded-transcript path and are left un-replayed until a
    /// later event or the in-flight fetch repairs the transcript.
    remote_delta_hydrations_in_flight: Arc<Mutex<HashSet<RemoteDeltaHydrationKey>>>,
    /// Remote register/upgrade lifecycle actions currently running. The UI
    /// disables buttons while pending, but this backend guard is the
    /// correctness boundary for duplicate browser tabs or direct API retries.
    remote_lifecycle_actions_in_flight: Arc<Mutex<HashSet<String>>>,
    terminal_local_command_semaphore: Arc<tokio::sync::Semaphore>,
    terminal_remote_command_semaphore: Arc<tokio::sync::Semaphore>,
    stopping_orchestrator_ids: Arc<Mutex<HashSet<String>>>,
    stopping_orchestrator_session_ids: Arc<Mutex<HashMap<String, HashSet<String>>>>,
    inner: Arc<StateMutex<StateInner>>,
    /// Test directories live until the final AppState clone drops. This field
    /// stays last so every store, SQLite connection, and state handle above is
    /// closed before Windows attempts recursive removal.
    #[cfg(test)]
    test_temp_root: Option<Arc<TestTempRoot>>,
}

#[cfg(test)]
impl AppState {
    fn test_temp_root_path(&self) -> Option<&FsPath> {
        self.test_temp_root.as_deref().map(TestTempRoot::path)
    }
}

const SESSION_NOT_RUNNING_CONFLICT_MESSAGE: &str = "session is not currently running";
const TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT: usize = 4;
const TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT: usize = 4;
const AGENT_READINESS_CACHE_TTL: Duration = Duration::from_secs(5);
const ACTIVE_TURN_FILE_CHANGE_GRACE: Duration = Duration::from_millis(750);

#[derive(Clone)]
struct AgentReadinessCache {
    snapshot: Vec<AgentReadiness>,
    expires_at: std::time::Instant,
    invalidated: bool,
}

impl AgentReadinessCache {
    fn fresh(snapshot: Vec<AgentReadiness>) -> Self {
        Self {
            snapshot,
            expires_at: std::time::Instant::now() + AGENT_READINESS_CACHE_TTL,
            invalidated: false,
        }
    }

    fn needs_refresh(&self, now: std::time::Instant) -> bool {
        self.invalidated || now >= self.expires_at
    }
}

fn fresh_agent_readiness_cache(default_workdir: &str) -> AgentReadinessCache {
    AgentReadinessCache::fresh(collect_agent_readiness(default_workdir))
}


/// Holds stop session options.
#[derive(Clone)]
struct StopSessionOptions {
    dispatch_queued_prompts_on_success: bool,
    pause_automatic_resumes_on_success: bool,
    orchestrator_stop_instance_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeStopOwnerKind {
    UserStop,
    EngramMcpRevocation,
    LostRuntimeTerminalization,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeStopOwner {
    kind: RuntimeStopOwnerKind,
    token: Option<RuntimeToken>,
    generation: u64,
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
fn bootstrap_default_local_state(default_workdir: &str) -> StateInner {
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
enum RuntimeMatchOutcome {
    Applied,
    SessionMissing,
    RuntimeMismatch,
}


/// Normalizes remote configs.
fn normalize_remote_configs(remotes: Vec<RemoteConfig>) -> Result<Vec<RemoteConfig>, ApiError> {
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
fn validate_persisted_remote_configs(remotes: Vec<RemoteConfig>) -> Result<Vec<RemoteConfig>> {
    normalize_remote_configs(remotes).map_err(|err| anyhow!(err.message))
}

/// Normalizes local user facing path.
fn normalize_local_user_facing_path(path: &str) -> String {
    normalize_user_facing_path(FsPath::new(path))
        .to_string_lossy()
        .into_owned()
}

/// Normalizes workspace layout paths.
fn normalize_workspace_layout_paths(layout: &mut WorkspaceLayoutDocument) {
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
fn collect_workspace_layout_summaries<'a>(
    layouts: impl Iterator<Item = &'a WorkspaceLayoutDocument>,
) -> Vec<WorkspaceLayoutSummary> {
    let mut workspaces = layouts
        .map(|layout| WorkspaceLayoutSummary {
            id: layout.id.clone(),
            revision: layout.revision,
            updated_at: layout.updated_at.clone(),
            control_panel_side: layout.control_panel_side,
            theme_id: layout.theme_id.clone(),
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
struct EngramProjectResetFences {
    next_generation: u64,
    owners: HashMap<String, u64>,
}

impl EngramProjectResetFences {
    fn contains(&self, project_id: &str) -> bool {
        self.owners.contains_key(project_id)
    }

    fn claim(&mut self, project_id: &str) -> Option<u64> {
        if self.contains(project_id) {
            return None;
        }
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let generation = self.next_generation;
        self.owners.insert(project_id.to_owned(), generation);
        Some(generation)
    }

    fn is_owned_by(&self, project_id: &str, generation: u64) -> bool {
        self.owners.get(project_id).copied() == Some(generation)
    }

    fn release(&mut self, project_id: &str, generation: u64) -> bool {
        if !self.is_owned_by(project_id, generation) {
            return false;
        }
        self.owners.remove(project_id);
        true
    }
}

/// Represents state inner.
struct StateInner {
    codex: CodexState,
    /// Process/runtime side of the optional Engram host adapter. Callers may
    /// clone this handle while holding `inner`, but must release the state
    /// mutex before invoking any control operation.
    engram_host_adapter: Arc<EngramHostAdapter>,
    /// Runtime-only cache of projects whose non-empty `.engram-project`
    /// marker was confirmed off the global state mutex. Hot locked paths may
    /// consult this set but must never touch the filesystem themselves.
    engram_declared_project_ids: HashSet<String>,
    /// Projects whose `.engram-project` marker has been observed in this
    /// process. The first observation seeds the cache; later changes reset
    /// already-created runtimes so their MCP configuration follows the marker.
    engram_declaration_checked_project_ids: HashSet<String>,
    /// Runtime-only project fence used while Engram connection settings drain
    /// the old sidecars. It prevents newly-created delegations from binding to
    /// the old connection between the reset snapshot and commit.
    engram_project_resets: EngramProjectResetFences,
    preferences: AppPreferences,
    /// Runtime retry marker for settings mutations that changed memory but
    /// failed synchronous persistence. An identical request must retry the
    /// write instead of reporting success merely because the values already
    /// match memory.
    settings_persist_dirty: bool,
    /// Tracks whether the dirty settings include remote routing so repeated
    /// failures keep the operator warning specific and accurate.
    remote_settings_persist_dirty: bool,
    /// A remote delta mutated in-memory state but its synchronous persistence
    /// attempt returned an error. Exact redelivery may now be a semantic no-op,
    /// so those paths must retry the full snapshot before advancing remote
    /// revision/replay bookkeeping.
    remote_delta_persist_dirty: bool,
    revision: u64,
    next_session_number: usize,
    next_message_number: u64,
    /// Stable project records used by local and remote session routing.
    projects: Vec<Project>,
    /// Host-level fail-closed ledger for Engram work-authority tuples that
    /// TermAl retired. Unlike project settings, this survives project deletion
    /// so recreating the same Engram store cannot reuse a retired credential.
    engram_retired_work_authority_grants: Vec<EngramRetiredWorkAuthorityGrant>,
    /// Durable outbox of project scopes whose coordination-board data must be
    /// fenced and removed after the project deletion reaches termal.sqlite.
    /// The dedicated cleanup worker removes an item in memory only after the
    /// fence/cascade succeeds, then wakes primary persistence to durably clear
    /// it; a crash leaves the on-disk item for the next boot.
    pending_coordination_scope_deletions: BTreeSet<String>,
    /// Durable outbox of deleted projects whose project-default response-board
    /// tabs must become ordinary custom tabs. Values retain the project's last
    /// live name so an idempotent retry cannot fall back to the creation-time
    /// tab label after the project record has disappeared.
    pending_response_board_project_detachments: BTreeMap<String, String>,
    ignored_discovered_codex_thread_ids: BTreeSet<String>,
    /// Broad remote state is metadata-only. Focused bounded-tail responses use
    /// `remote_session_transcript_applied_revisions` so a same-revision delta is
    /// suppressed only for the session whose retained suffix was materialized.
    /// Tracks the latest remote revision applied for each mirrored remote so
    /// stale snapshots and deltas cannot roll local proxy state backward.
    remote_applied_revisions: HashMap<String, u64>,
    /// Tracks the latest broad full-state snapshot applied for each mirrored
    /// remote. Cleared with `remote_applied_revisions` and the per-remote replay
    /// cache when event-stream continuity is lost.
    remote_snapshot_applied_revisions: HashMap<String, u64>,
    /// Tracks focused bounded-tail materialization per remote session.
    remote_session_transcript_applied_revisions: HashMap<String, HashMap<String, u64>>,
    /// Session records carry the serializable session plus runtime-only state.
    sessions: Vec<SessionRecord>,
    /// Durable session rows isolated during startup validation.
    ///
    /// These records are deliberately absent from `sessions` so corrupted
    /// payloads cannot appear healthy, but full-snapshot fallback persistence
    /// must preserve their SQLite rows for recovery instead of treating them as
    /// user-deleted sessions.
    quarantined_persisted_session_ids: BTreeSet<String>,
    /// Runtime instances for orchestrator templates live beside ordinary sessions.
    orchestrator_instances: Vec<OrchestratorInstance>,
    /// Durable parent-child delegation links for ordinary sessions.
    delegations: Vec<DelegationRecord>,
    /// Durable delegation rows isolated during startup validation. See
    /// `quarantined_persisted_session_ids` for the preservation contract.
    quarantined_persisted_delegation_ids: BTreeSet<String>,
    /// Pending parent resumes waiting on one or more child delegation results.
    delegation_waits: Vec<DelegationWaitRecord>,
    /// Runtime-only mutation stamps for delegation rows. The background persist
    /// worker compares these with its watermark to upsert only delegation rows
    /// that changed since the last successful write.
    delegation_mutation_stamps: BTreeMap<String, u64>,
    /// Runtime-only delegation tombstones drained by the persist thread and
    /// restored on write failure, mirroring session tombstone retry behavior.
    removed_delegation_ids: BTreeMap<String, u64>,
    /// Indexes currently running read-only delegation records by `delegations` index.
    running_read_only_delegations: BTreeSet<usize>,
    /// Server-backed workspace documents keyed by workspace id.
    workspace_layouts: BTreeMap<String, WorkspaceLayoutDocument>,
    /// Monotonic counter used to stamp mutated session records. The persist
    /// thread uses this plus its own watermark to write only the sessions
    /// that changed since the last successful persist, so each
    /// `commit_locked` pays only for the sessions it actually touched
    /// rather than rewriting every session row. Starts at 0; advanced by
    /// [`StateInner::next_mutation_stamp`] on every `session_mut*` call.
    last_mutation_stamp: u64,
    /// Session ids removed from `sessions` since the last persist tick.
    /// Drained by the persist thread and applied as targeted `DELETE`
    /// statements so removed rows do not linger after the move to
    /// delta persistence.
    removed_session_ids: Vec<String>,
}

impl StateInner {
    /// Creates a new instance.
    fn new() -> Self {
        Self {
            codex: CodexState::default(),
            engram_host_adapter: Arc::new(EngramHostAdapter::default()),
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
            projects: Vec::new(),
            engram_retired_work_authority_grants: Vec::new(),
            pending_coordination_scope_deletions: BTreeSet::new(),
            pending_response_board_project_detachments: BTreeMap::new(),
            ignored_discovered_codex_thread_ids: BTreeSet::new(),
            remote_applied_revisions: HashMap::new(),
            remote_snapshot_applied_revisions: HashMap::new(),
            remote_session_transcript_applied_revisions: HashMap::new(),
            sessions: Vec::new(),
            quarantined_persisted_session_ids: BTreeSet::new(),
            orchestrator_instances: Vec::new(),
            delegations: Vec::new(),
            quarantined_persisted_delegation_ids: BTreeSet::new(),
            delegation_waits: Vec::new(),
            delegation_mutation_stamps: BTreeMap::new(),
            removed_delegation_ids: BTreeMap::new(),
            running_read_only_delegations: BTreeSet::new(),
            workspace_layouts: BTreeMap::new(),
            last_mutation_stamp: 0,
            removed_session_ids: Vec::new(),
        }
    }

    /// Returns whether the supplied remote snapshot revision is stale for this remote.
    fn should_skip_remote_applied_revision(&self, remote_id: &str, remote_revision: u64) -> bool {
        self.remote_applied_revisions
            .get(remote_id)
            .is_some_and(|latest_revision| *latest_revision >= remote_revision)
    }

    /// Returns whether the supplied remote state snapshot revision is stale for
    /// this remote. Same-revision snapshots are allowed after ordinary deltas so
    /// repair paths can materialize state after a sibling delta at that revision.
    fn should_skip_remote_applied_snapshot_revision(
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
    fn should_skip_remote_applied_delta_revision(
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
    fn should_skip_remote_session_applied_delta_revision(
        &self,
        remote_id: &str,
        remote_session_id: &str,
        remote_revision: u64,
    ) -> bool {
        self.should_skip_remote_applied_delta_revision(remote_id, remote_revision)
            || self
                .remote_session_transcript_applied_revisions
                .get(remote_id)
                .and_then(|sessions| sessions.get(remote_session_id))
                .is_some_and(|latest_revision| *latest_revision >= remote_revision)
    }

    /// Records the latest applied remote revision for a mirrored remote.
    fn note_remote_applied_revision(&mut self, remote_id: &str, remote_revision: u64) {
        self.remote_applied_revisions
            .entry(remote_id.to_owned())
            .and_modify(|latest_revision| {
                *latest_revision = (*latest_revision).max(remote_revision);
            })
            .or_insert(remote_revision);
    }

    /// Records that a broad full-state snapshot, not just a narrow delta or
    /// focused session response, has materialized this remote revision.
    fn note_remote_applied_snapshot_revision(&mut self, remote_id: &str, remote_revision: u64) {
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
    fn note_remote_session_transcript_applied_revision(
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
struct EngramMcpInstalledDescriptor {
    binary_path: String,
    home: String,
    /// Exact principal installed into the live agent process/thread and its
    /// Engram MCP child. Runtime-reported model synchronization must not
    /// silently change this frozen identity.
    actor_id: String,
    actor_context: Option<String>,
    store_key: Option<EngramAuthorityStoreKey>,
    work_authority_grant: Option<String>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct EngramAuthorityRevocationTarget {
    binary_path: String,
    home: String,
    project_root: String,
    store_key: Option<EngramAuthorityStoreKey>,
    work_authority_grant: String,
}

impl std::fmt::Debug for EngramAuthorityRevocationTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngramAuthorityRevocationTarget")
            .field("binary_path", &self.binary_path)
            .field("home", &self.home)
            .field("project_root", &self.project_root)
            .field("store_key", &self.store_key)
            .field("work_authority_grant", &"[REDACTED]")
            .finish()
    }
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
            .field(
                "work_authority_grant",
                &self.work_authority_grant.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// Represents a session record.
#[derive(Clone)]
struct SessionRecord {
    active_codex_approval_policy: Option<CodexApprovalPolicy>,
    active_codex_reasoning_effort: Option<CodexReasoningEffort>,
    active_codex_sandbox_mode: Option<CodexSandboxMode>,
    /// Process-local identity of the currently active turn. Incremented for
    /// every dispatch so asynchronous callbacks cannot mistake a successor
    /// turn on the same shared runtime for the turn that armed them.
    active_turn_generation: u64,
    /// Exact durable mailbox boundary represented by the active turn, if any.
    /// Failure/Stop recovery takes this value before draining the next queued
    /// turn so it never requeues a successor's delivery.
    active_turn_mailbox_notification: Option<MailboxNotificationDelivery>,
    active_turn_start_message_count: Option<usize>,
    active_turn_file_changes: BTreeMap<String, WorkspaceFileChangeKind>,
    active_turn_file_change_grace_deadline: Option<std::time::Instant>,
    agent_commands: Vec<AgentCommand>,
    codex_approval_policy: CodexApprovalPolicy,
    codex_reasoning_effort: CodexReasoningEffort,
    codex_sandbox_mode: CodexSandboxMode,
    external_session_id: Option<String>,
    pending_claude_approvals: HashMap<String, ClaudePendingApproval>,
    pending_claude_user_inputs: HashMap<String, ClaudePendingUserInput>,
    pending_codex_approvals: HashMap<String, CodexPendingApproval>,
    pending_codex_user_inputs: HashMap<String, CodexPendingUserInput>,
    pending_codex_mcp_elicitations: HashMap<String, CodexPendingMcpElicitation>,
    pending_codex_app_requests: HashMap<String, CodexPendingAppRequest>,
    pending_acp_approvals: HashMap<String, AcpPendingApproval>,
    /// Arrival order for ACP permission requests. The map above owns the
    /// routing payload; this queue makes the protocol's multiple-pending
    /// behavior deterministic and prevents later requests from overtaking the
    /// first unresolved approval.
    pending_acp_approval_order: VecDeque<String>,
    /// FIFO follow-up prompts collected while the runtime is busy.
    queued_prompts: VecDeque<QueuedPromptRecord>,
    /// Original peer messages represented by a coalesced queued prompt, keyed
    /// by that prompt's stable id. Ordinary queued prompts have no entry.
    queued_peer_messages: HashMap<String, Vec<PendingPrompt>>,
    /// Global transcript position represented by `session.messages[0]`.
    ///
    /// Persisted sessions load only a bounded recent tail into memory. New
    /// sessions and fully localized remote sessions start at zero. Message
    /// deltas translate local vec indices through this offset so browser
    /// cursors keep using stable whole-transcript positions.
    message_start_index: usize,
    message_positions: HashMap<String, usize>,
    /// Present only for proxy sessions mirrored from a remote TermAl backend.
    remote_id: Option<String>,
    remote_session_id: Option<String>,
    runtime: SessionRuntime,
    /// Descriptor actually installed into the currently attached local agent
    /// runtime/thread. This process-local capability record is deliberately
    /// omitted from persistence and wire snapshots; it exists so a later
    /// clear/disable/delete can revoke a superseded grant against the binary
    /// and home that received it, and so Engram control/context work keeps the
    /// exact actor identity installed into that runtime.
    engram_mcp_installed: Option<EngramMcpInstalledDescriptor>,
    runtime_reset_required: bool,
    /// A stale Engram MCP runtime whose process termination failed and whose
    /// handle is retained for retry. Unlike an ordinary settings reset, its
    /// natural exit must preserve the automatic-resume safety latch.
    engram_mcp_runtime_quarantined: bool,
    /// Persisted explicit-resume guard. User Stop and failed stop persistence
    /// both block mailbox/workflow auto-dispatch until a manual user turn
    /// resets the latch in `start_turn_on_record`.
    orchestrator_auto_dispatch_blocked: bool,
    runtime_stop_in_progress: bool,
    /// Token-scoped owner for the runtime callback fence. The generation
    /// prevents a stale Stop/revocation completion from releasing a newer
    /// owner's fence after the runtime has been replaced.
    runtime_stop_owner: Option<RuntimeStopOwner>,
    runtime_stop_generation: u64,
    /// An immediate Engram MCP revocation arrived while an ordinary Stop owned
    /// the runtime callback fence. Stop completion consumes this marker: a
    /// successful Stop has no runtime left to revoke, while a failed Stop
    /// transfers its fence directly to the revocation teardown.
    engram_mcp_revocation_pending: bool,
    /// Terminal callbacks deferred while `runtime_stop_in_progress` was true. Each callback keeps
    /// the active-turn generation it observed so replay cannot land on a successor turn that reused
    /// the same persistent runtime token. Replayed in arrival order on dedicated stop failure so the
    /// session doesn't get stuck in a stale Active state or reconstruct the wrong terminal sequence
    /// when completion/error and runtime-exit both land during the shutdown window.
    deferred_stop_callbacks: Vec<DeferredStopCallback>,
    /// Host-private Engram binding and open-turn state. It is persisted in the
    /// session row but never copied onto the wire-level `Session`.
    engram: EngramSessionState,
    /// Process-local readiness fence while boot restores this session's
    /// host-private Engram routing authority. The wire projection exposes the
    /// fence, but it is deliberately absent from persisted `Session` state so
    /// every process recomputes recovery work from durable authority records.
    engram_boot_recovery_pending: bool,
    /// A queue-drain activation that arrived while the boot-recovery fence was
    /// raised. Recovery completion consumes this bit and re-kicks the queue;
    /// persisted user queues with no current-process activation stay dormant.
    engram_boot_recovery_dispatch_pending: bool,
    /// Suppresses duplicate first-use retry workers after eager boot recovery
    /// exhausts its wall-clock budget. Process-local and never persisted.
    engram_boot_recovery_retry_in_progress: bool,
    hidden: bool,
    session: Session,
    /// Monotonic mutation stamp assigned by [`StateInner::next_mutation_stamp`]
    /// every time this record is handed out through one of the
    /// `session_mut*` helpers. Not persisted — stamps start at `0` on each
    /// process lifetime and the persist thread's watermark advances
    /// accordingly. A stamp strictly greater than the persist watermark
    /// means this record has in-memory changes that have not yet reached
    /// SQLite.
    mutation_stamp: u64,
    /// Last global mutation stamp at which the bounded composer history
    /// changed. The persist delta uses this independent watermark to keep the
    /// potentially large history out of hot session-metadata rewrites.
    prompt_history_mutation_stamp: u64,
}

/// Pre-promotion state captured before a queued prompt is promoted to an
/// active turn, so a failed persist commit can put the record back exactly
/// where it was instead of leaving a half-started turn in memory: head gone,
/// latch cleared, status Active, and nothing delivered to the runtime.
struct QueuePromotionSnapshot {
    status: SessionStatus,
    preview: String,
    live_activity: Option<SessionLiveActivity>,
    auto_dispatch_blocked: bool,
    active_turn_mailbox_notification: Option<MailboxNotificationDelivery>,
    active_turn_start_message_count: Option<usize>,
    // Appending the prompt message also appends composer history and resyncs
    // the retained-transcript projections; the rollback must undo those too.
    prompt_history: Vec<String>,
    prompt_history_mutation_stamp: u64,
    message_count: u32,
    messages_loaded: bool,
}

impl SessionRecord {
    fn clear_runtime(&mut self) {
        self.runtime = SessionRuntime::None;
        self.engram_mcp_installed = None;
    }

    fn capture_queue_promotion_snapshot(&self) -> QueuePromotionSnapshot {
        QueuePromotionSnapshot {
            status: self.session.status,
            preview: self.session.preview.clone(),
            live_activity: self.session.live_activity.clone(),
            auto_dispatch_blocked: self.orchestrator_auto_dispatch_blocked,
            active_turn_mailbox_notification: self.active_turn_mailbox_notification.clone(),
            active_turn_start_message_count: self.active_turn_start_message_count,
            prompt_history: self.session.prompt_history.clone(),
            prompt_history_mutation_stamp: self.prompt_history_mutation_stamp,
            message_count: self.session.message_count,
            messages_loaded: self.session.messages_loaded,
        }
    }

    /// Undoes a promotion whose persist commit failed: drops the prompt
    /// message the promotion appended, returns the queue head to the front,
    /// abandons the pending Engram dispatch, and restores the captured
    /// fields including the explicit-resume latch. A runtime spawned for the
    /// attempt is left attached; an idle session with a live runtime is an
    /// ordinary state and the next promotion reuses it.
    fn restore_queue_promotion(
        &mut self,
        snapshot: QueuePromotionSnapshot,
        queued: QueuedPromptRecord,
        started_message_id: &str,
    ) {
        if let Some(position) = self
            .session
            .messages
            .iter()
            .rposition(|message| message.id() == started_message_id)
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
        take_and_abandon_engram_pending_dispatch(self);
        self.queued_prompts.push_front(queued);
        sync_pending_prompts(self);
        self.session.status = snapshot.status;
        self.session.preview = snapshot.preview;
        self.session.live_activity = snapshot.live_activity;
        self.active_turn_mailbox_notification = snapshot.active_turn_mailbox_notification;
        self.active_turn_start_message_count = snapshot.active_turn_start_message_count;
        self.set_auto_dispatch_blocked(snapshot.auto_dispatch_blocked);
    }

    /// Sets the explicit-resume latch and mirrors it onto the embedded
    /// session shape in the same step. The record flag is the authority;
    /// wire projections (`wire_session_from_record`, the metadata summary)
    /// re-derive `queue_paused` from it on every build, so the mirror here
    /// only keeps the in-record copy honest for readers that inspect
    /// `record.session` directly. Production code must flip the latch only
    /// through this helper.
    fn set_auto_dispatch_blocked(&mut self, blocked: bool) {
        self.orchestrator_auto_dispatch_blocked = blocked;
        self.session.queue_paused = blocked;
    }

    fn clear_runtime_reset(&mut self) {
        self.runtime_reset_required = false;
        self.engram_mcp_runtime_quarantined = false;
    }

    fn claim_runtime_stop(
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
    fn claim_missing_runtime_stop(&mut self, kind: RuntimeStopOwnerKind) -> u64 {
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

    fn clear_runtime_stop(&mut self) {
        self.runtime_stop_in_progress = false;
        self.runtime_stop_owner = None;
        self.engram_mcp_revocation_pending = false;
    }

    fn runtime_stop_is_owned_by(
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


    fn missing_runtime_stop_is_owned_by(
        &self,
        kind: RuntimeStopOwnerKind,
        generation: u64,
    ) -> bool {
        self.runtime_stop_owner.as_ref().is_some_and(|owner| {
            owner.kind == kind && owner.token.is_none() && owner.generation == generation
        })
    }

    /// Returns the paired remote identity when this record is a valid proxy.
    fn remote_proxy_identity(&self) -> Result<Option<(&str, &str)>> {
        validate_remote_proxy_identity(
            self.remote_id.as_deref(),
            self.remote_session_id.as_deref(),
        )
    }

    /// Returns whether remote proxy.
    fn is_remote_proxy(&self) -> bool {
        self.remote_proxy_identity()
            .is_ok_and(|identity| identity.is_some())
    }

    /// Returns whether this record has the canonical local identity.
    ///
    /// An invalid partial remote identity is deliberately neither local nor a
    /// remote proxy, so local-only paths fail closed instead of acting on it.
    fn is_local_session(&self) -> bool {
        self.remote_proxy_identity()
            .is_ok_and(|identity| identity.is_none())
    }
}



/// Handles Codex approval policy from JSON value.
fn codex_approval_policy_from_json_value(value: &Value) -> Option<CodexApprovalPolicy> {
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
fn codex_reasoning_effort_from_json_value(value: &Value) -> Option<CodexReasoningEffort> {
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
fn codex_sandbox_mode_from_json_value(value: &Value) -> Option<CodexSandboxMode> {
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
fn default_forked_codex_session_name(current_name: &str, thread_name: Option<&str>) -> String {
    let trimmed_thread_name = thread_name.map(str::trim).filter(|value| !value.is_empty());
    let trimmed_current_name = current_name.trim();
    let base = trimmed_thread_name.unwrap_or(trimmed_current_name);
    format!("{base} Fork")
}

/// Resolves forked Codex working directory.
fn resolve_forked_codex_workdir(
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
