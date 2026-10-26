// Owns bounded, memory-only timing observations for registration and rescans.
// Does not decide admission, durability, index contents, or test credit.
// Split from test_run_registration.rs's console-only diagnostics.
const TEST_RUN_TRACE_CAPACITY: usize = 256;
const TEST_RUN_TRACE_PHASES: usize = 32;
const TEST_RUN_RECENT_RESCAN_LIMIT: usize = 20;

#[derive(Clone, Default)]
struct TestRunPhaseTraces(Arc<Mutex<TestRunPhaseRing>>);

struct TestRunPhaseRing {
    epoch: std::time::Instant,
    entries: VecDeque<TestRunPhaseSnapshot>,
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
struct TestRunPhaseSnapshot {
    trace_id: String,
    operation_id: String,
    run_id: Option<String>,
    kind: &'static str,
    phases: Vec<TestRunPhasePoint>,
    completed: bool,
    truncated: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunPhasePoint {
    phase: &'static str,
    monotonic_micros: u64,
}

struct TestRunPhaseTrace {
    ring: TestRunPhaseTraces,
    trace_id: String,
}

impl TestRunPhaseTraces {
    fn begin(
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
        });
        let trace = TestRunPhaseTrace {
            ring: self.clone(),
            trace_id,
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

    fn recent_rescans(&self, limit: usize) -> (Vec<TestRunPhaseSnapshot>, bool) {
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

    fn selected(
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
    fn mark(&self, phase: &'static str) {
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
struct TestRunPhaseQuery {
    run_id: Option<String>,
    operation_id: Option<String>,
    recent: Option<TestRunRecentTraces>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum TestRunRecentTraces {
    Rescans,
}

async fn get_test_run_phase_traces(
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
        "limitations": "Memory-only bounded observations; response_ready is not proof of delivery. Timings share this host's monotonic epoch and reset on restart. Evicted traces are absent; recent rescans are newest first."
    })))
}
