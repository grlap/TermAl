// Owns bounded, memory-only timing observations for registration and rescans.
// Does not decide admission, durability, index contents, or test credit.
// Split from test_run_registration.rs's console-only diagnostics.

use super::*;
pub(crate) const TEST_RUN_TRACE_CAPACITY: usize = 256;
pub(crate) const TEST_RUN_TRACE_PHASES: usize = 32;
pub(crate) const TEST_RUN_RECENT_RESCAN_LIMIT: usize = 20;

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_RUN_PHASE_FLUSH_CHECK: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Default)]
pub(crate) struct TestRunPhaseTraces(pub(crate) Arc<Mutex<TestRunPhaseRing>>);

pub(crate) struct TestRunPhaseRing {
    pub(crate) epoch: std::time::Instant,
    pub(crate) entries: VecDeque<TestRunPhaseSnapshot>,
}

impl Default for TestRunPhaseRing {
    fn default() -> Self {
        Self {
            epoch: std::time::Instant::now(),
            entries: VecDeque::new(),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestRunPhaseSnapshot {
    trace_id: String,
    pub(crate) operation_id: String,
    pub(crate) run_id: Option<String>,
    pub(crate) kind: &'static str,
    pub(crate) phases: Vec<TestRunPhasePoint>,
    pub(crate) completed: bool,
    pub(crate) truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) state_lock: Option<StateLockBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rescan: Option<TestRunRescanTrace>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestRunPhasePoint {
    pub(crate) phase: &'static str,
    pub(crate) monotonic_micros: u64,
}

pub(crate) struct TestRunPhaseTrace {
    pub(crate) ring: TestRunPhaseTraces,
    trace_id: String,
    pub(crate) epoch: std::time::Instant,
}

// Stack-owned while StateInner is held. Recording cannot allocate, format,
// traverse state, or acquire the ring. Only flush, after unlock, touches it.
pub(crate) struct TestRunPhaseBuffer {
    pub(crate) epoch: std::time::Instant,
    pub(crate) points: [Option<TestRunPhasePoint>; TEST_RUN_TRACE_PHASES],
    pub(crate) len: usize,
    pub(crate) truncated: bool,
}

impl TestRunPhaseBuffer {
    pub(crate) fn mark(&mut self, phase: &'static str) {
        self.mark_at(phase, std::time::Instant::now());
    }

    pub(crate) fn mark_at(&mut self, phase: &'static str, at: std::time::Instant) {
        if self.len == self.points.len() {
            self.truncated = true;
            return;
        }
        self.points[self.len] = Some(TestRunPhasePoint {
            phase,
            monotonic_micros: at
                .saturating_duration_since(self.epoch)
                .as_micros()
                .min(u128::from(u64::MAX)) as u64,
        });
        self.len += 1;
    }
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestRunRescanTrace {
    pub(crate) origin: &'static str,
    pub(crate) roots: usize,
    pub(crate) scanned: usize,
    pub(crate) indexed_before: usize,
    pub(crate) projected: usize,
    pub(crate) sessions: usize,
    pub(crate) registrations: usize,
    pub(crate) observations: usize,
    pub(crate) compacted: usize,
    pub(crate) removed: usize,
    pub(crate) changed: usize,
    pub(crate) restored: usize,
    pub(crate) published_events: usize,
    pub(crate) publication_failed: bool,
    pub(crate) cards: usize,
    pub(crate) compaction_population: usize,
    pub(crate) compaction_eligible: usize,
    pub(crate) persistence: TestRunTracePersistence,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestRunTracePersistence {
    pub(crate) background_queued: usize,
    pub(crate) synchronous: usize,
    pub(crate) failed: usize,
    // Calls hidden inside existing helpers are not guessed from worker liveness.
    pub(crate) unobserved_helpers: usize,
}

impl TestRunTracePersistence {
    pub(crate) fn merge(&mut self, other: Self) {
        self.background_queued += other.background_queued;
        self.synchronous += other.synchronous;
        self.failed += other.failed;
        self.unobserved_helpers += other.unobserved_helpers;
    }
    pub(crate) fn observe(&mut self, dispatch: Result<PersistDispatch, ()>) {
        match dispatch {
            Ok(PersistDispatch::BackgroundQueued) => self.background_queued += 1,
            Ok(PersistDispatch::Synchronous) => self.synchronous += 1,
            Err(()) => self.failed += 1,
        }
    }

    pub(crate) fn observe_compaction(&mut self, result: &Result<bool>) {
        self.observe(match result {
            Ok(true) => Ok(PersistDispatch::Synchronous),
            Ok(false) => Ok(PersistDispatch::BackgroundQueued),
            Err(_) => Err(()),
        });
    }
}

impl TestRunPhaseTraces {
    pub(crate) fn begin(
        &self,
        kind: &'static str,
        run_id: Option<&str>,
        operation_id: &str,
        started: std::time::Instant,
    ) -> TestRunPhaseTrace {
        let trace_id = Uuid::new_v4().to_string();
        let mut ring = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let point = TestRunPhasePoint {
            phase: "received",
            monotonic_micros: started
                .saturating_duration_since(ring.epoch)
                .as_micros()
                .min(u128::from(u64::MAX)) as u64,
        };
        if ring.entries.len() == TEST_RUN_TRACE_CAPACITY {
            ring.entries.pop_front();
        }
        ring.entries.push_back(TestRunPhaseSnapshot {
            trace_id: trace_id.clone(),
            operation_id: test_run_diagnostic_id(operation_id),
            run_id: run_id.map(test_run_diagnostic_id),
            kind,
            phases: vec![point],
            completed: false,
            truncated: false,
            state_lock: None,
            rescan: None,
        });
        let trace = TestRunPhaseTrace {
            ring: self.clone(),
            trace_id,
            epoch: ring.epoch,
        };
        drop(ring);
        #[cfg(test)]
        if kind == "registration" {
            if let Some(run_id) = run_id {
                // The actual received point is stored before releasing a scan.
                test_history_registration_received(run_id);
            }
        }
        trace
    }

    pub(crate) fn recent_rescans(&self, limit: usize) -> (Vec<TestRunPhaseSnapshot>, bool) {
        let ring = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut recent = ring
            .entries
            .iter()
            .rev()
            .filter(|entry| entry.kind == "rescan");
        let traces = recent.by_ref().take(limit).cloned().collect();
        (traces, recent.next().is_some())
    }

    pub(crate) fn selected(
        &self,
        run_id: Option<&str>,
        operation_id: Option<&str>,
    ) -> Vec<TestRunPhaseSnapshot> {
        let ring = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let matches = |entry: &TestRunPhaseSnapshot| {
            run_id.is_none_or(|id| entry.run_id.as_deref() == Some(id))
                && operation_id.is_none_or(|id| entry.operation_id == id)
        };
        let now = ring.epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        let windows: Vec<_> = ring
            .entries
            .iter()
            .filter(|entry| matches(entry))
            .map(|entry| {
                (
                    entry.phases[0].monotonic_micros,
                    if entry.completed {
                        entry.phases.last().unwrap().monotonic_micros
                    } else {
                        now
                    },
                )
            })
            .collect();
        ring.entries
            .iter()
            .filter(|entry| {
                matches(entry)
                    || (entry.kind == "rescan" && {
                        let start = entry.phases[0].monotonic_micros;
                        let end = if entry.completed {
                            entry.phases.last().unwrap().monotonic_micros
                        } else {
                            now
                        };
                        windows
                            .iter()
                            .any(|(left, right)| start <= *right && end >= *left)
                    })
            })
            .cloned()
            .collect()
    }
}

impl TestRunPhaseTrace {
    pub(crate) fn buffer(&self) -> TestRunPhaseBuffer {
        TestRunPhaseBuffer {
            epoch: self.epoch,
            points: [None; TEST_RUN_TRACE_PHASES],
            len: 0,
            truncated: false,
        }
    }

    pub(crate) fn flush(
        &self,
        buffer: &mut TestRunPhaseBuffer,
        rescan: Option<&TestRunRescanTrace>,
    ) {
        #[cfg(test)]
        TEST_RUN_PHASE_FLUSH_CHECK.with(|check| {
            if let Some(check) = check.borrow().as_ref() {
                check();
            }
        });
        let mut ring = self
            .ring
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = ring
            .entries
            .iter_mut()
            .find(|entry| entry.trace_id == self.trace_id)
        {
            entry.truncated |= buffer.truncated;
            for point in buffer.points[..buffer.len].iter().flatten() {
                if entry.phases.len() < TEST_RUN_TRACE_PHASES {
                    entry.phases.push(*point);
                } else {
                    entry.truncated = true;
                }
            }
            if let Some(rescan) = rescan {
                entry.rescan = Some(rescan.clone());
            }
        }
        buffer.len = 0;
        buffer.truncated = false;
    }

    pub(crate) fn retain_state_lock(&self, block: &StateLockBlock) -> bool {
        let Ok(mut ring) = self.ring.0.try_lock() else {
            return false;
        };
        let Some(entry) = ring
            .entries
            .iter_mut()
            .find(|entry| entry.trace_id == self.trace_id)
        else {
            return false;
        };
        entry.state_lock = Some(block.clone());
        true
    }

    pub(crate) fn mark(&self, phase: &'static str) {
        let mut ring = self
            .ring
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let at = ring.epoch.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        if let Some(entry) = ring
            .entries
            .iter_mut()
            .find(|entry| entry.trace_id == self.trace_id)
        {
            if entry.phases.len() < TEST_RUN_TRACE_PHASES {
                entry.phases.push(TestRunPhasePoint {
                    phase,
                    monotonic_micros: at,
                });
            } else {
                entry.truncated = true;
                if phase == "finished" {
                    *entry.phases.last_mut().unwrap() = TestRunPhasePoint {
                        phase,
                        monotonic_micros: at,
                    };
                }
            }
        }
    }
}

impl Drop for TestRunPhaseTrace {
    fn drop(&mut self) {
        self.mark("finished");
        let mut ring = self
            .ring
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = ring
            .entries
            .iter_mut()
            .find(|entry| entry.trace_id == self.trace_id)
        {
            entry.completed = true;
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TestRunPhaseQuery {
    pub(crate) run_id: Option<String>,
    pub(crate) operation_id: Option<String>,
    pub(crate) recent: Option<TestRunRecentTraces>,
    pub(crate) limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TestRunRecentTraces {
    Rescans,
}

pub(crate) async fn get_test_run_phase_traces(
    State(state): State<AppState>,
    query: Result<Query<TestRunPhaseQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(|error| api_query_rejection("test run diagnostics", error))?;
    let valid = |value: &str| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    };
    let (traces, selection_limit, selection_truncated) = if query.recent.is_some() {
        if query.run_id.is_some() || query.operation_id.is_some() {
            return Err(ApiError::bad_request(
                "recent rescans cannot be combined with exact identifiers",
            ));
        }
        let limit = query.limit.unwrap_or(TEST_RUN_RECENT_RESCAN_LIMIT);
        if !(1..=TEST_RUN_TRACE_CAPACITY).contains(&limit) {
            return Err(ApiError::bad_request("limit must be between 1 and 256"));
        }
        let (traces, truncated) = state.test_run_phase_traces.recent_rescans(limit);
        (traces, Some(limit), truncated)
    } else {
        if query.limit.is_some() {
            return Err(ApiError::bad_request("limit requires recent=rescans"));
        }
        if (query.run_id.is_none() && query.operation_id.is_none())
            || query.run_id.as_deref().is_some_and(|value| !valid(value))
            || query
                .operation_id
                .as_deref()
                .is_some_and(|value| !valid(value))
        {
            return Err(ApiError::bad_request(
                "an exact runId or operationId is required",
            ));
        }
        (
            state
                .test_run_phase_traces
                .selected(query.run_id.as_deref(), query.operation_id.as_deref()),
            None,
            false,
        )
    };
    Ok(Json(json!({
        "serverInstanceId": state.server_instance_id,
        "capacity": TEST_RUN_TRACE_CAPACITY, "phaseLimit": TEST_RUN_TRACE_PHASES,
        "traces": traces,
        "selectionLimit": selection_limit, "selectionTruncated": selection_truncated,
        "limitations": "Memory-only bounded observations; response_ready is not proof of delivery. Timings share this host's monotonic epoch and reset on restart. Evicted traces are absent; recent rescans are newest first. Rescan held phases are buffered until unlock; a missing release point or truncated trace is not a complete lock span. Persistence counts observe returned metadata, identity-proof and card dispatches only; unobservedHelpers counts other helper calls, not their unknown persistence outcomes."
    })))
}
