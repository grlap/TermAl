// Counts resident closing custody without reporting job bodies or changing work.
// Adopted days describe turn adoption, not terminal-failure or retry timestamps.
// One pass borrows jobs under StateInner; no store access or job cloning occurs.

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClosingJobStates {
    terminal: u64,
    retrying: u64,
}

impl ClosingJobStates {
    fn count(&mut self, terminal: bool) {
        if terminal {
            self.terminal += 1;
        } else {
            self.retrying += 1;
        }
    }
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClosingJobReasons {
    focus_scope_changed: u64,
    source_refused: u64,
    max_attempts: u64,
    other: u64,
}

impl ClosingJobReasons {
    fn count(&mut self, message: &str, attempts: u32) {
        // Both scope-change producers emit this constant prefix:
        // EngramRuntimeClosingScope::require_current (unadmitted_capture) and
        // runtime_observation_scope_changed (unadmitted_closing).
        if message.starts_with("not reported: scope changed; pinned [") {
            self.focus_scope_changed += 1;
        // Closing's Legacy(ConfiguredGitFilters) error uses this exact Display
        // message from SourceBasisRefusal. UnsupportedGitlinks instead produces
        // a report, so its text is not inferred to be a terminal source refusal.
        } else if message == "review verification does not run configured Git filters" {
            self.source_refused += 1;
        } else if attempts >= ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS {
            self.max_attempts += 1;
        } else {
            self.other += 1;
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClosingJobCounts {
    server_instance_id: String,
    revision: u64,
    total: ClosingJobStates,
    // Only terminal jobs have a terminal-path reason; retry errors are excluded.
    by_reason: ClosingJobReasons,
    adopted_day: BTreeMap<String, ClosingJobStates>,
}

async fn get_closing_job_counts(
    State(state): State<AppState>,
) -> Result<Json<ClosingJobCounts>, ApiError> {
    run_blocking_api(move || {
        let server_instance_id = state.server_instance_id.clone();
        let inner = state.inner.lock().expect("state mutex poisoned");
        let mut counts = ClosingJobCounts {
            server_instance_id,
            revision: inner.revision,
            total: ClosingJobStates::default(),
            by_reason: ClosingJobReasons::default(),
            adopted_day: BTreeMap::new(),
        };
        for job in inner
            .sessions
            .iter()
            .flat_map(|record| record.engram.unadmitted_closing_jobs.iter())
        {
            let terminal = job.terminal_failure.is_some();
            counts.total.count(terminal);
            if let Some(message) = job.terminal_failure.as_deref() {
                counts.by_reason.count(message, job.attempts);
            }
            let day = chrono::DateTime::parse_from_rfc3339(&job.trace.adopted_at)
                .map(|at| at.with_timezone(&chrono::Utc).date_naive().to_string())
                .unwrap_or_else(|_| "unknown".to_owned());
            counts.adopted_day.entry(day).or_default().count(terminal);
        }
        Ok(Json(counts))
    })
    .await
}
