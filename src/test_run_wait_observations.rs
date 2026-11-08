// Host observations in the existing run wait. Owns source identity and lookup,
// not command recognition, artifact judging, verification or a separate queue.

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunWaitObservationLabel {
    execution_id: String,
    runtime_identity: Option<String>,
    tool_identity: String,
    command_fingerprint: String,
    turn_generation: u64,
    project_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestRunWaitObservationUnknownReason {
    Terminal,
    Interrupted,
    Unavailable,
}

impl TestRunWaitObservationLabel {
    fn from_row(row: &TestRunCommandObservation) -> Self {
        Self {
            execution_id: row.execution_id.clone(),
            runtime_identity: row.runtime_identity.clone(),
            tool_identity: row.tool_identity.clone(),
            command_fingerprint: row.command_fingerprint.clone(),
            turn_generation: row.turn_generation,
            project_id: row.summary.project_id.clone(),
        }
    }
}

// Resolve the actual execution identity, including installed retained proof.
// No global overview, artifact rescan or prefix alone can invent a row.
fn test_run_wait_exact_observation<'a>(
    inner: &'a StateInner,
    id: &str,
) -> Option<&'a TestRunCommandObservation> {
    if let Some(row) = inner.test_run_observations.get(id) {
        return (row.execution_id == id && row.summary.run_id == id).then_some(row);
    }
    let mut retained = inner
        .test_run_compacted
        .index()
        .execution(id)
        .filter_map(|key| inner.test_run_compacted.get(key))
        .filter(|row| test_run_compact_is_installed(inner, row))
        .flat_map(|row| &row.observations)
        .filter(|row| row.execution_id == id && row.summary.run_id == id);
    let row = retained.next()?;
    retained.next().is_none().then_some(row)
}

fn test_run_wait_observation_run_label(row: &TestRunCommandObservation) -> TestRunWaitRunLabel {
    TestRunWaitRunLabel {
        remote: None,
        observation: Some(TestRunWaitObservationLabel::from_row(row)),
        run_id: row.execution_id.clone(),
        run_dir: String::new(),
        preset: row.summary.preset,
        worktree: row.summary.worktree.clone(),
        owner_session_id: row.summary.owner_session_id.clone(),
        started_at: row.summary.started_at.clone(),
        request_identity: None,
        locator_identity: None,
        registration_backed: false,
        registration_progress: TestRunWaitRegistrationProgress::default(),
    }
}

fn test_run_wait_matching_observation<'a>(
    inner: &'a StateInner,
    label: &TestRunWaitRunLabel,
) -> Option<&'a TestRunCommandObservation> {
    let row = test_run_wait_exact_observation(inner, &label.run_id)?;
    (label.observation.as_ref() == Some(&TestRunWaitObservationLabel::from_row(row))
        && row.summary.owner_session_id == label.owner_session_id
        && row.summary.started_at == label.started_at
        && row.summary.worktree == label.worktree)
        .then_some(row)
}

fn test_run_wait_observation_verdict(
    inner: &StateInner,
    label: &TestRunWaitRunLabel,
    project_removed: bool,
) -> Option<TestRunWaitVerdict> {
    let Some(row) = test_run_wait_matching_observation(inner, label).filter(|_| !project_removed)
    else {
        return Some(TestRunWaitVerdict::UnknownObservation(
            TestRunWaitObservationUnknownReason::Unavailable,
        ));
    };
    // A declared producer publishes END before its off-lock artifact read
    // finishes. Its exit-derived state is provisional until application.
    if row.declared_verdict_pending {
        return row.monitoring_interrupted_at.as_ref().map(|_| {
            TestRunWaitVerdict::UnknownObservation(TestRunWaitObservationUnknownReason::Interrupted)
        });
    }
    // Consume only the observation's factual terminal state. In particular,
    // a background declared execution can still end UNKNOWN; artifact judging
    // belongs to the producer, not the wait. No exit or PASS is synthesized.
    if row.summary.ended_at.is_some() {
        return test_run_wait_verdict(Some(&row.summary), false).or(Some(
            TestRunWaitVerdict::UnknownObservation(TestRunWaitObservationUnknownReason::Terminal),
        ));
    }
    row.monitoring_interrupted_at.as_ref().map(|_| {
        TestRunWaitVerdict::UnknownObservation(TestRunWaitObservationUnknownReason::Interrupted)
    })
}

fn test_run_wait_observation_section(
    label: &TestRunWaitRunLabel,
    summary: Option<&TestRunSummary>,
    verdict: Option<TestRunWaitVerdict>,
) -> String {
    format!(
        "### Run `{}` (host observed)\n- Verdict: {}\n- Exit code: {}\n- Started: {}\n- Ended: {}\n- Source: host observation; no launcher directory or verification credit is inferred.\n- Inspect: GET /api/test-runs/{}",
        label.run_id,
        test_run_wait_verdict_label(verdict),
        summary
            .and_then(|run| run.exit_code)
            .map_or_else(|| "none".to_owned(), |code| code.to_string()),
        summary
            .and_then(|run| run.started_at.as_deref())
            .or(label.started_at.as_deref())
            .unwrap_or("unknown"),
        summary
            .and_then(|run| run.ended_at.as_deref())
            .unwrap_or("unknown"),
        label.run_id,
    )
}
