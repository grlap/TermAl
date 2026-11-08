// Presentation derived from the existing canonical test-run projection.
// Counts and fixed-size session activity replace run rows on global transport;
// they do not settle runs, own notifications, or discard history or evidence.

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunCounts {
    total: usize,
    running: usize,
    passed: usize,
    failed: usize,
    unknown: usize,
    needs_attention: usize,
    evidence_at_risk: usize,
    evidence_safe: usize,
    evidence_unknown: usize,
}

impl TestRunCounts {
    fn include(&mut self, run: &TestRunSummary, protected: &HashSet<String>) {
        self.total += 1;
        match run.state {
            TestRunState::Running => self.running += 1,
            TestRunState::Passed => self.passed += 1,
            TestRunState::Failed => self.failed += 1,
            TestRunState::Unknown => self.unknown += 1,
        }
        self.needs_attention += usize::from(test_run_needs_attention(run, protected));
        match run.registration.restart_would_void_evidence {
            Some(true) => self.evidence_at_risk += 1,
            Some(false) => self.evidence_safe += 1,
            None => self.evidence_unknown += 1,
        }
    }
}

fn test_run_needs_attention(run: &TestRunSummary, protected: &HashSet<String>) -> bool {
    matches!(run.state, TestRunState::Running | TestRunState::Unknown)
        || run.registration.restart_would_void_evidence != Some(false)
        || test_run_summary_ids(run).any(|id| protected.contains(id))
}

fn test_run_attention_ids(inner: &StateInner) -> HashSet<String> {
    let mut protected = test_run_waited_ids(inner);
    protected.extend(
        inner
            .test_run_cards
            .iter()
            .filter(|(_, card)| !card.terminal)
            .map(|(id, _)| id.clone()),
    );
    protected.extend(inner.test_runs.registration_failures.iter().cloned());
    protected.extend(inner.test_runs.observations_failed.iter().cloned());
    protected.extend(
        inner
            .test_runs
            .observation_persistence
            .0
            .lock()
            .expect("test observation mutex poisoned")
            .keys()
            .cloned(),
    );
    protected
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectTestRunCounts {
    // None is the explicit unassigned bucket, not an all-project total.
    project_id: Option<String>,
    #[serde(flatten)]
    counts: TestRunCounts,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunOverview {
    version: u64,
    ready: bool,
    #[serde(flatten)]
    counts: TestRunCounts,
    projects: Vec<ProjectTestRunCounts>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionTestRunActivity {
    owned_running: usize,
    foreground_running: usize,
    background_running: usize,
    mode_unknown_running: usize,
    notify_running: usize,
    foreground_stage: Option<String>,
    owned_stage: Option<String>,
    notify_stage: Option<String>,
}

impl SessionTestRunActivity {
    fn include_owned(&mut self, run: &TestRunSummary) {
        self.owned_running += 1;
        self.owned_stage = (self.owned_running == 1)
            .then(|| run.current_stage.clone())
            .flatten();
        if run.detached == Some(false) {
            self.foreground_running += 1;
            self.foreground_stage = (self.foreground_running == 1)
                .then(|| run.current_stage.clone())
                .flatten();
        } else {
            self.background_running += 1;
            self.mode_unknown_running += usize::from(
                run.detached.is_none()
                    && run.registration.registration_state.as_deref() == Some("hostObserved"),
            );
        }
    }

    fn include_notify(&mut self, run: &TestRunSummary) {
        self.notify_running += 1;
        self.notify_stage = (self.notify_running == 1)
            .then(|| run.current_stage.clone())
            .flatten();
    }
}

// This output contains no run identity arrays or history. Only sessions with
// running work have entries; every entry has the same bounded shape.
fn test_run_overview(
    runs: &[TestRunSummary],
    ready: bool,
    version: u64,
    protected: &HashSet<String>,
) -> (TestRunOverview, BTreeMap<String, SessionTestRunActivity>) {
    let mut counts = TestRunCounts::default();
    let mut projects = BTreeMap::<Option<String>, TestRunCounts>::new();
    let mut activity = BTreeMap::<String, SessionTestRunActivity>::new();
    for run in runs {
        counts.include(run, protected);
        projects
            .entry(run.project_id.clone())
            .or_default()
            .include(run, protected);
        if run.state != TestRunState::Running {
            continue;
        }
        if let Some(owner) = &run.owner_session_id {
            activity
                .entry(owner.clone())
                .or_default()
                .include_owned(run);
        }
        if let Some(notify) = &run.notify_session_id {
            if run.owner_session_id.as_ref() != Some(notify) {
                activity
                    .entry(notify.clone())
                    .or_default()
                    .include_notify(run);
            }
        }
    }
    (
        TestRunOverview {
            version,
            ready,
            counts,
            projects: projects
                .into_iter()
                .map(|(project_id, counts)| ProjectTestRunCounts { project_id, counts })
                .collect(),
        },
        activity,
    )
}
