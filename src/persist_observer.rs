// Bounded in-memory persistence telemetry beside persist_tick_timings.rs.
// Owns numeric observations and a read-only HTTP projection, not scheduling,
// deadlines, acknowledgements, durable data or retry decisions. No paths,
// transcript payloads enter this observer. Session-local write rejections have
// one bounded diagnostic (session id and concrete error), never a history store.

use super::*;

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
pub(crate) struct PersistObservedCommit {
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
pub(crate) fn persist_commit_transaction_observed(
    tx: rusqlite::Transaction<'_>,
) -> rusqlite::Result<()> {
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
pub(crate) struct PersistObservedCounts {
    #[cfg(test)]
    pub(crate) metadata_cloned_fields: Vec<&'static str>,
    pub(crate) drained_requests: usize,
    pub(crate) accepted_fences: usize,
    pub(crate) pending_fences_before: usize,
    pub(crate) pending_fences_after: usize,
    /// From construction immediately before submission, including any caller
    /// delay before send: an upper bound, not exact channel wait.
    pub(crate) new_fence_age_max_us: u64,
    pub(crate) changed_sessions: usize,
    pub(crate) changed_delegations: usize,
    removed_sessions: usize,
    removed_delegations: usize,
    deferred_sessions: usize,
    deferred_delegations: usize,
    pub(crate) retained_messages: usize,
    pub(crate) payload_measured: bool,
    pub(crate) metadata_bytes: usize,
    pub(crate) metadata_fields: Vec<PersistMetadataFieldObservation>,
    pub(crate) metadata_transaction_committed: bool,
    pub(crate) session_metadata_bytes: usize,
    pub(crate) message_bytes: usize,
    prompt_history_bytes: usize,
    delegation_bytes: usize,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistMetadataFieldObservation {
    pub(crate) field: &'static str,
    pub(crate) units: usize,
    pub(crate) encoded_bytes: usize,
    pub(crate) attempted_upserts: usize,
    pub(crate) attempted_bytes: usize,
    attempted_deletes: usize,
    pub(crate) committed_upserts: usize,
    pub(crate) committed_bytes: usize,
    pub(crate) committed_deletes: usize,
}

pub(crate) struct PersistMetadataUnitObservation(pub(crate) Vec<PersistMetadataFieldObservation>);

impl PersistMetadataUnitObservation {
    pub(crate) fn new(units: &PersistMetadataUnits) -> Self {
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

    pub(crate) fn upsert(&mut self, field: &str, bytes: usize) {
        if let Some(row) = self.0.iter_mut().find(|row| row.field == field) {
            row.attempted_upserts += 1;
            row.attempted_bytes = row.attempted_bytes.saturating_add(bytes);
        }
    }

    pub(crate) fn delete(&mut self, field: &str) {
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

pub(crate) fn persist_observe_metadata_commit() {
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
pub(crate) struct PersistObservedPhases {
    pub(crate) drain_us: u64,
    pub(crate) collect_us: u64,
    pub(crate) collect_wait_us: u64,
    pub(crate) collect_hold_us: u64,
    pub(crate) collect_lock_count: usize,
    pub(crate) serialize_us: u64,
    pub(crate) open_us: u64,
    #[cfg(test)]
    pub(crate) test_open_connection_us: Option<u64>,
    #[cfg(test)]
    pub(crate) test_open_path_checks_us: Option<u64>,
    #[cfg(test)]
    pub(crate) test_open_reused_connection: Option<bool>,
    pub(crate) ticket_wait_us: u64,
    pub(crate) statements_us: u64,
    pub(crate) commit_us: u64,
    pub(crate) post_commit_us: u64,
    pub(crate) acknowledge_us: u64,
    pub(crate) bookkeeping_us: u64,
}

pub(crate) fn persist_observed_us(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistObservedTick {
    pub(crate) tick: u64,
    pub(crate) success: bool,
    pub(crate) loop_wall_us: u64,
    pub(crate) counts: PersistObservedCounts,
    pub(crate) phases: PersistObservedPhases,
    pub(crate) commit: PersistObservedCommit,
}

pub(crate) struct PersistObservedActive {
    pub(crate) started: std::time::Instant,
    pub(crate) phase_started: std::time::Instant,
    pub(crate) phase: &'static str,
    pub(crate) tick: PersistObservedTick,
}

#[derive(Default)]
pub(crate) struct PersistObserverState {
    next_tick: u64,
    pub(crate) completed_ticks: u64,
    pub(crate) failed_ticks: u64,
    pub(crate) rejected_session_writes: u64,
    pub(crate) last_session_rejection: Option<PersistSessionRejection>,
    pub(crate) active: Option<PersistObservedActive>,
    pub(crate) recent: std::collections::VecDeque<PersistObservedTick>,
    pub(crate) slowest: Vec<PersistObservedTick>,
    // Since the host lifecycle heartbeat last took them.
    heartbeat_payload_bytes: u64,
    heartbeat_longest_tick_us: u64,
}

/// The writer's totals and its since-last-heartbeat payload and slowest tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PersistHeartbeatCounters {
    pub(crate) completed_ticks: u64,
    pub(crate) failed_ticks: u64,
    pub(crate) payload_bytes: u64,
    pub(crate) longest_tick_us: u64,
}

#[derive(Default)]
pub(crate) struct PersistObserver {
    pub(crate) state: Mutex<PersistObserverState>,
    // Only an explicitly armed disposable fixture retains every completed
    // sample. Production keeps its existing bounded recent/slowest windows.
    #[cfg(test)]
    pub(crate) test_tick_sink:
        Mutex<Option<mpsc::Sender<(std::time::Instant, PersistObservedTick)>>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistObserverSnapshot {
    pub(crate) version: u8,
    recent_limit: usize,
    slowest_limit: usize,
    pub(crate) completed_ticks: u64,
    pub(crate) failed_ticks: u64,
    pub(crate) rejected_session_writes: u64,
    pub(crate) last_session_rejection: Option<PersistSessionRejection>,
    /// std::sync::mpsc has no instantaneous depth; do not mislabel the
    /// drained-request or accepted-fence counts as a channel depth.
    pub(crate) channel_queue_depth: Option<usize>,
    pub(crate) current: Option<PersistObserverCurrent>,
    pub(crate) recent: Vec<PersistObservedTick>,
    /// Greatest loop wall time first; equal times prefer the newest tick.
    pub(crate) slowest: Vec<PersistObservedTick>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistObserverCurrent {
    pub(crate) phase: &'static str,
    pub(crate) age_us: u64,
    pub(crate) phase_age_us: u64,
    pub(crate) sample: PersistObservedTick,
}

impl PersistObserver {
    pub(crate) fn record_session_rejections(&self, rejected: &[PersistSessionRejection]) {
        let Some(last) = rejected.last() else { return };
        self.with_state(|state| {
            state.rejected_session_writes = state
                .rejected_session_writes
                .saturating_add(rejected.len() as u64);
            state.last_session_rejection = Some(last.bounded_observation());
        });
    }

    pub(crate) fn with_state<R>(&self, work: impl FnOnce(&mut PersistObserverState) -> R) -> R {
        // Diagnostic poisoning must not change the writer's result.
        work(&mut self.state.lock().unwrap_or_else(|error| error.into_inner()))
    }

    pub(crate) fn begin_at(self: &Arc<Self>, now: std::time::Instant) -> PersistObservationScope {
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

    pub(crate) fn update(&self, work: impl FnOnce(&mut PersistObservedTick)) {
        self.with_state(|state| {
            if let Some(active) = &mut state.active {
                work(&mut active.tick);
            }
        });
    }

    pub(crate) fn phase(&self, phase: &'static str) {
        self.with_state(|state| {
            if let Some(active) = &mut state.active {
                active.phase = phase;
                active.phase_started = std::time::Instant::now();
            }
        });
    }

    pub(crate) fn collection_lock(&self, waited: Duration, held: Duration) {
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

    pub(crate) fn finish_at(
        &self,
        success: bool,
        timings: &PersistTickTimings,
        now: std::time::Instant,
    ) {
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
            let payload = [
                counts.metadata_bytes,
                counts.session_metadata_bytes,
                counts.message_bytes,
                counts.prompt_history_bytes,
                counts.delegation_bytes,
            ]
            .into_iter()
            .fold(0u64, |total, bytes| total.saturating_add(bytes as u64));
            state.heartbeat_payload_bytes = state.heartbeat_payload_bytes.saturating_add(payload);
            state.heartbeat_longest_tick_us = state
                .heartbeat_longest_tick_us
                .max(active.tick.loop_wall_us);
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
            #[cfg(test)]
            if let Some(sink) = self.test_tick_sink.lock().unwrap().as_ref() {
                let _ = sink.send((now, active.tick.clone()));
            }
            state.recent.push_back(active.tick);
        });
    }

    /// Totals plus the payload and slowest tick since the previous call,
    /// which resets those two. Only the observer mutex is taken.
    pub(crate) fn take_heartbeat_counters(&self) -> PersistHeartbeatCounters {
        self.with_state(|state| PersistHeartbeatCounters {
            completed_ticks: state.completed_ticks,
            failed_ticks: state.failed_ticks,
            payload_bytes: std::mem::take(&mut state.heartbeat_payload_bytes),
            longest_tick_us: std::mem::take(&mut state.heartbeat_longest_tick_us),
        })
    }

    pub(crate) fn snapshot_at(&self, now: std::time::Instant) -> PersistObserverSnapshot {
        self.with_state(|state| PersistObserverSnapshot {
            version: 1,
            recent_limit: PERSIST_OBSERVER_RECENT_LIMIT,
            slowest_limit: PERSIST_OBSERVER_SLOWEST_LIMIT,
            completed_ticks: state.completed_ticks,
            failed_ticks: state.failed_ticks,
            rejected_session_writes: state.rejected_session_writes,
            last_session_rejection: state.last_session_rejection.clone(),
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
    pub(crate) static PERSIST_OBSERVATION: std::cell::RefCell<Option<Arc<PersistObserver>>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn persist_observe(work: impl FnOnce(&PersistObserver)) {
    PERSIST_OBSERVATION.with(|slot| {
        if let Some(observer) = slot.borrow().as_ref() {
            work(observer);
        }
    });
}

pub(crate) fn persist_observe_phase(phase: &'static str) {
    persist_observe(|observer| observer.phase(phase));
}

pub(crate) fn persist_observe_session_rejections(rejected: &[PersistSessionRejection]) {
    persist_observe(|observer| observer.record_session_rejections(rejected));
}

pub(crate) struct PersistObservationScope {
    pub(crate) observer: Arc<PersistObserver>,
    pub(crate) previous: Option<Arc<PersistObserver>>,
    pub(crate) finished: bool,
}

impl PersistObservationScope {
    pub(crate) fn finish(mut self, success: bool, timings: &PersistTickTimings) {
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
pub(crate) fn persist_collect_locked<R>(
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

pub(crate) fn persist_observe_delta(delta: &PersistDelta) {
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

pub(crate) fn persist_observe_payload(
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

pub(crate) async fn get_persistence_diagnostics(State(state): State<AppState>) -> Json<Value> {
    // Only the bounded observer mutex and atomics; no StateInner, SQLite or
    // file reads. `stateBroadcast` counts the SSE broadcaster's diffs and full
    // states (state_broadcast_diff.rs).
    Json(json!({ "serverInstanceId": state.server_instance_id,
        "writer": state.persist_observer.snapshot_at(std::time::Instant::now()),
        "stateBroadcast": state.state_broadcast_senders.diff_counters.snapshot() }))
}
