// Bounded in-memory persistence telemetry beside persist_tick_timings.rs.
// Owns numeric observations and a read-only HTTP projection, not scheduling,
// deadlines, acknowledgements, durable data or retry decisions. No paths,
// payloads, persisted identifiers or error text enter this observer.

const PERSIST_OBSERVER_RECENT_LIMIT: usize = 16;
const PERSIST_OBSERVER_SLOWEST_LIMIT: usize = 8;

/// A measured zero is distinct from an unobservable or unattempted phase.
#[derive(Clone, Default, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum PersistObservedMetric {
    #[default]
    Unavailable,
    Measured {
        value: u64,
    },
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistObservedCommit {
    sqlite_call_us: PersistObservedMetric,
    transaction_cleanup_us: PersistObservedMetric,
    // SQLite's implicit autocheckpoint is inside COMMIT. Observing it via a
    // replacement WAL hook would take ownership of checkpoint policy. Keep
    // these explicitly unavailable rather than pretending they cost zero.
    wal_write_us: PersistObservedMetric,
    fsync_us: PersistObservedMetric,
    checkpoint_us: PersistObservedMetric,
    wal_frames: PersistObservedMetric,
    checkpointed_frames: PersistObservedMetric,
}

/// Same COMMIT statement and Transaction drop as rusqlite's commit(), with
/// separate boundaries so a failed COMMIT's rollback is charged to cleanup.
/// No checkpoint query, callback, policy or error conversion is introduced.
fn persist_commit_transaction_observed(tx: rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    let started = std::time::Instant::now();
    let result = tx.execute_batch("COMMIT");
    let committed = std::time::Instant::now();
    persist_observe_phase("commit_cleanup");
    let cleanup_started = std::time::Instant::now();
    drop(tx);
    let cleaned = std::time::Instant::now();
    persist_observe(|observer| {
        observer.update(|tick| {
            tick.commit.sqlite_call_us = PersistObservedMetric::Measured {
                value: persist_observed_us(committed.duration_since(started)),
            };
            tick.commit.transaction_cleanup_us = PersistObservedMetric::Measured {
                value: persist_observed_us(cleaned.duration_since(cleanup_started)),
            };
        })
    });
    result
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistObservedCounts {
    drained_requests: usize,
    accepted_fences: usize,
    pending_fences_before: usize,
    pending_fences_after: usize,
    /// From construction immediately before submission, including any caller
    /// delay before send: an upper bound, not exact channel wait.
    new_fence_age_max_us: u64,
    changed_sessions: usize,
    changed_delegations: usize,
    removed_sessions: usize,
    removed_delegations: usize,
    deferred_sessions: usize,
    deferred_delegations: usize,
    retained_messages: usize,
    payload_measured: bool,
    metadata_bytes: usize,
    metadata_fields: Vec<PersistMetadataFieldObservation>,
    metadata_transaction_committed: bool,
    session_metadata_bytes: usize,
    message_bytes: usize,
    prompt_history_bytes: usize,
    delegation_bytes: usize,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistMetadataFieldObservation {
    field: &'static str,
    units: usize,
    encoded_bytes: usize,
    attempted_upserts: usize,
    attempted_bytes: usize,
    attempted_deletes: usize,
    committed_upserts: usize,
    committed_bytes: usize,
    committed_deletes: usize,
}

struct PersistMetadataUnitObservation(Vec<PersistMetadataFieldObservation>);

impl PersistMetadataUnitObservation {
    fn new(units: &PersistMetadataUnits) -> Self {
        let mut rows: Vec<_> = METADATA_UNIT_FIELDS
            .iter()
            .map(|field| PersistMetadataFieldObservation {
                field,
                ..Default::default()
            })
            .collect();
        for ((field, _), (_, encoded)) in units {
            if let Some(row) = rows.iter_mut().find(|row| row.field == field.as_str()) {
                row.units += 1;
                row.encoded_bytes = row.encoded_bytes.saturating_add(encoded.len());
            }
        }
        Self(rows)
    }

    fn upsert(&mut self, field: &str, bytes: usize) {
        if let Some(row) = self.0.iter_mut().find(|row| row.field == field) {
            row.attempted_upserts += 1;
            row.attempted_bytes = row.attempted_bytes.saturating_add(bytes);
        }
    }

    fn delete(&mut self, field: &str) {
        if let Some(row) = self.0.iter_mut().find(|row| row.field == field) {
            row.attempted_deletes += 1;
        }
    }
}

impl Drop for PersistMetadataUnitObservation {
    fn drop(&mut self) {
        persist_observe(|observer| {
            observer.update(|tick| {
                tick.counts.metadata_bytes = self.0.iter().map(|row| row.attempted_bytes).sum();
                tick.counts.metadata_fields = std::mem::take(&mut self.0);
                tick.counts.metadata_transaction_committed = false;
            })
        });
    }
}

fn persist_observe_metadata_commit() {
    persist_observe(|observer| {
        observer.update(|tick| {
            tick.counts.metadata_transaction_committed = true;
            for field in &mut tick.counts.metadata_fields {
                field.committed_upserts = field.attempted_upserts;
                field.committed_bytes = field.attempted_bytes;
                field.committed_deletes = field.attempted_deletes;
            }
        })
    });
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistObservedPhases {
    drain_us: u64,
    collect_us: u64,
    collect_wait_us: u64,
    collect_hold_us: u64,
    collect_lock_count: usize,
    serialize_us: u64,
    open_us: u64,
    ticket_wait_us: u64,
    statements_us: u64,
    commit_us: u64,
    post_commit_us: u64,
    acknowledge_us: u64,
    bookkeeping_us: u64,
}

fn persist_observed_us(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistObservedTick {
    tick: u64,
    success: bool,
    loop_wall_us: u64,
    counts: PersistObservedCounts,
    phases: PersistObservedPhases,
    commit: PersistObservedCommit,
}

struct PersistObservedActive {
    started: std::time::Instant,
    phase_started: std::time::Instant,
    phase: &'static str,
    tick: PersistObservedTick,
}

#[derive(Default)]
struct PersistObserverState {
    next_tick: u64,
    completed_ticks: u64,
    failed_ticks: u64,
    active: Option<PersistObservedActive>,
    recent: std::collections::VecDeque<PersistObservedTick>,
    slowest: Vec<PersistObservedTick>,
    // Since the host lifecycle heartbeat last took them.
    heartbeat_payload_bytes: u64,
    heartbeat_longest_tick_us: u64,
}

/// The writer's totals and its since-last-heartbeat payload and slowest tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PersistHeartbeatCounters {
    completed_ticks: u64,
    failed_ticks: u64,
    payload_bytes: u64,
    longest_tick_us: u64,
}

#[derive(Default)]
struct PersistObserver {
    state: Mutex<PersistObserverState>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistObserverSnapshot {
    version: u8,
    recent_limit: usize,
    slowest_limit: usize,
    completed_ticks: u64,
    failed_ticks: u64,
    /// std::sync::mpsc has no instantaneous depth; do not mislabel the
    /// drained-request or accepted-fence counts as a channel depth.
    channel_queue_depth: Option<usize>,
    current: Option<PersistObserverCurrent>,
    recent: Vec<PersistObservedTick>,
    /// Greatest loop wall time first; equal times prefer the newest tick.
    slowest: Vec<PersistObservedTick>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistObserverCurrent {
    phase: &'static str,
    age_us: u64,
    phase_age_us: u64,
    sample: PersistObservedTick,
}

impl PersistObserver {
    fn with_state<R>(&self, work: impl FnOnce(&mut PersistObserverState) -> R) -> R {
        // Diagnostic poisoning must not change the writer's result.
        work(&mut self.state.lock().unwrap_or_else(|error| error.into_inner()))
    }

    fn begin_at(self: &Arc<Self>, now: std::time::Instant) -> PersistObservationScope {
        self.with_state(|state| {
            state.next_tick = state.next_tick.saturating_add(1);
            state.active = Some(PersistObservedActive {
                started: now,
                phase_started: now,
                phase: "drain",
                tick: PersistObservedTick {
                    tick: state.next_tick,
                    ..Default::default()
                },
            });
        });
        let previous = PERSIST_OBSERVATION.with(|slot| slot.replace(Some(Arc::clone(self))));
        PersistObservationScope {
            observer: Arc::clone(self),
            previous,
            finished: false,
        }
    }

    fn update(&self, work: impl FnOnce(&mut PersistObservedTick)) {
        self.with_state(|state| {
            if let Some(active) = &mut state.active {
                work(&mut active.tick);
            }
        });
    }

    fn phase(&self, phase: &'static str) {
        self.with_state(|state| {
            if let Some(active) = &mut state.active {
                active.phase = phase;
                active.phase_started = std::time::Instant::now();
            }
        });
    }

    fn collection_lock(&self, waited: Duration, held: Duration) {
        self.update(|tick| {
            tick.phases.collect_wait_us = tick
                .phases
                .collect_wait_us
                .saturating_add(persist_observed_us(waited));
            tick.phases.collect_hold_us = tick
                .phases
                .collect_hold_us
                .saturating_add(persist_observed_us(held));
            tick.phases.collect_lock_count = tick.phases.collect_lock_count.saturating_add(1);
        });
    }

    fn finish_at(&self, success: bool, timings: &PersistTickTimings, now: std::time::Instant) {
        self.with_state(|state| {
            let Some(mut active) = state.active.take() else {
                return;
            };
            active.tick.success = success;
            active.tick.loop_wall_us =
                persist_observed_us(now.saturating_duration_since(active.started));
            let p = &mut active.tick.phases;
            p.collect_us = persist_observed_us(timings.collect);
            p.serialize_us = persist_observed_us(timings.serialize);
            p.open_us = persist_observed_us(timings.open);
            p.ticket_wait_us = persist_observed_us(timings.ticket_wait);
            p.statements_us = persist_observed_us(timings.statements);
            p.commit_us = persist_observed_us(timings.commit);
            p.post_commit_us = persist_observed_us(timings.post_commit);
            p.acknowledge_us = persist_observed_us(timings.acknowledge);
            state.completed_ticks = state.completed_ticks.saturating_add(1);
            state.failed_ticks = state.failed_ticks.saturating_add(u64::from(!success));
            let counts = &active.tick.counts;
            let payload = [counts.metadata_bytes, counts.session_metadata_bytes, counts.message_bytes,
                counts.prompt_history_bytes, counts.delegation_bytes]
                .into_iter()
                .fold(0u64, |total, bytes| total.saturating_add(bytes as u64));
            state.heartbeat_payload_bytes = state.heartbeat_payload_bytes.saturating_add(payload);
            state.heartbeat_longest_tick_us = state.heartbeat_longest_tick_us.max(active.tick.loop_wall_us);
            if state.recent.len() == PERSIST_OBSERVER_RECENT_LIMIT {
                state.recent.pop_front();
            }
            state.slowest.push(active.tick.clone());
            state.slowest.sort_by(|a, b| {
                b.loop_wall_us
                    .cmp(&a.loop_wall_us)
                    .then(b.tick.cmp(&a.tick))
            });
            state.slowest.truncate(PERSIST_OBSERVER_SLOWEST_LIMIT);
            state.recent.push_back(active.tick);
        });
    }

    /// Totals plus the payload and slowest tick since the previous call,
    /// which resets those two. Only the observer mutex is taken.
    fn take_heartbeat_counters(&self) -> PersistHeartbeatCounters {
        self.with_state(|state| PersistHeartbeatCounters {
            completed_ticks: state.completed_ticks,
            failed_ticks: state.failed_ticks,
            payload_bytes: std::mem::take(&mut state.heartbeat_payload_bytes),
            longest_tick_us: std::mem::take(&mut state.heartbeat_longest_tick_us),
        })
    }

    fn snapshot_at(&self, now: std::time::Instant) -> PersistObserverSnapshot {
        self.with_state(|state| PersistObserverSnapshot {
            version: 1,
            recent_limit: PERSIST_OBSERVER_RECENT_LIMIT,
            slowest_limit: PERSIST_OBSERVER_SLOWEST_LIMIT,
            completed_ticks: state.completed_ticks,
            failed_ticks: state.failed_ticks,
            channel_queue_depth: None,
            current: state.active.as_ref().map(|active| PersistObserverCurrent {
                phase: active.phase,
                age_us: persist_observed_us(now.saturating_duration_since(active.started)),
                phase_age_us: persist_observed_us(
                    now.saturating_duration_since(active.phase_started),
                ),
                sample: active.tick.clone(),
            }),
            recent: state.recent.iter().cloned().collect(),
            slowest: state.slowest.clone(),
        })
    }
}

thread_local! {
    static PERSIST_OBSERVATION: std::cell::RefCell<Option<Arc<PersistObserver>>> = const { std::cell::RefCell::new(None) };
}

fn persist_observe(work: impl FnOnce(&PersistObserver)) {
    PERSIST_OBSERVATION.with(|slot| {
        if let Some(observer) = slot.borrow().as_ref() {
            work(observer);
        }
    });
}

fn persist_observe_phase(phase: &'static str) {
    persist_observe(|observer| observer.phase(phase));
}

struct PersistObservationScope {
    observer: Arc<PersistObserver>,
    previous: Option<Arc<PersistObserver>>,
    finished: bool,
}

impl PersistObservationScope {
    fn finish(mut self, success: bool, timings: &PersistTickTimings) {
        self.observer
            .finish_at(success, timings, std::time::Instant::now());
        self.finished = true;
    }
}

impl Drop for PersistObservationScope {
    fn drop(&mut self) {
        if !self.finished {
            self.observer.finish_at(
                false,
                &PersistTickTimings::default(),
                std::time::Instant::now(),
            );
        }
        PERSIST_OBSERVATION.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

/// Uses the original collection lock/operation; the observer mutex is never
/// held across StateInner acquisition or copying. Outside a writer tick it
/// follows the original path without collecting times.
#[track_caller]
fn persist_collect_locked<R>(
    inner: &StateMutex<StateInner>,
    work: impl FnOnce(&mut StateInner) -> R,
) -> R {
    let observer = PERSIST_OBSERVATION.with(|slot| slot.borrow().clone());
    let Some(observer) = observer else {
        return work(&mut inner.lock().expect("state mutex poisoned"));
    };
    let requested = std::time::Instant::now();
    let mut state = inner.lock().expect("state mutex poisoned");
    let acquired = std::time::Instant::now();
    let result = work(&mut state);
    drop(state);
    let released = std::time::Instant::now();
    observer.collection_lock(
        acquired.duration_since(requested),
        released.duration_since(acquired),
    );
    result
}

fn persist_observe_delta(delta: &PersistDelta) {
    persist_observe(|observer| {
        observer.update(|tick| {
            tick.counts.changed_sessions = delta.changed_sessions.len();
            tick.counts.changed_delegations =
                delta.changed_delegations.as_ref().map_or(0, Vec::len);
            tick.counts.removed_sessions = delta.removed_session_ids.len();
            tick.counts.removed_delegations = delta.removed_delegation_ids.len();
            tick.counts.deferred_sessions = delta.deferred_session_ids.len();
            tick.counts.deferred_delegations = delta.deferred_delegation_ids.len();
        })
    });
}

fn persist_observe_payload(
    metadata: &str,
    sessions: &[SerializedPersistedSession],
    delegations: &[(&str, String)],
) {
    persist_observe(|observer| {
        observer.update(|tick| {
            tick.counts.payload_measured = true;
            tick.counts.metadata_bytes = metadata.len();
            tick.counts.session_metadata_bytes = sessions.iter().map(|s| s.value_json.len()).sum();
            tick.counts.retained_messages = sessions.iter().map(|s| s.messages.len()).sum();
            tick.counts.message_bytes = sessions
                .iter()
                .flat_map(|s| &s.messages)
                .map(|m| m.value_json.len())
                .sum();
            tick.counts.prompt_history_bytes = sessions
                .iter()
                .filter_map(|s| s.prompt_history_value_json.as_ref())
                .map(String::len)
                .sum();
            tick.counts.delegation_bytes = delegations.iter().map(|(_, json)| json.len()).sum();
        })
    });
}

async fn get_persistence_diagnostics(State(state): State<AppState>) -> Json<Value> {
    // Only the bounded observer mutex; no StateInner, SQLite or file reads.
    Json(json!({ "serverInstanceId": state.server_instance_id,
        "writer": state.persist_observer.snapshot_at(std::time::Instant::now()) }))
}
