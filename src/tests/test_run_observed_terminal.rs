// Test-only child of test_run_registration.rs, using disposable run fixtures.
// Owns stale observed execution visibility, exact launcher correlation and
// terminal-history regressions; does not run providers or award verification.
use super::*;

const LAUNCH: &str = "node scripts/test-launcher.mjs full";

fn start(fixture: &RunFixture, session: &str, key: &str, command: &str) -> String {
    fixture.state.note_engram_command_started(
        session,
        &EngramObservationProvenance::Ambient,
        key,
        Some(command),
        Some(&test_run_display_path(&fixture.root)),
    );
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .values()
        .find(|row| row.tool_identity == sha256_hex(key.as_bytes()))
        .unwrap()
        .execution_id
        .clone()
}

fn launcher(fixture: &RunFixture, session: &str) -> (String, PathBuf) {
    let id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(
        &fs::canonicalize(&fixture.root).unwrap(),
        &test_run_timestamp(),
    );
    request["owner"] = json!(session);
    request["creatorPid"] = json!(std::process::id());
    let directory = write_run(
        &fixture.runs_dir(),
        &id,
        request,
        Some(running_results(Some(717))),
    );
    fixture
        .state
        .register_test_run(session, registration_request(&id, &directory))
        .unwrap();
    (id, directory)
}

fn finish_artifact(directory: &FsPath, id: &str, success: bool) {
    let mut results = passed_results();
    results["runId"] = json!(id);
    results["ended"] = json!(test_run_timestamp());
    if !success {
        results["state"] = json!("failed");
        results["exitCode"] = json!(101);
        results["stages"][1]["state"] = json!("failed");
        results["stages"][1]["code"] = json!(101);
    }
    fs::write(directory.join("results.json"), results.to_string()).unwrap();
}

fn assert_no_credit(fixture: &RunFixture) {
    let inner = fixture.state.inner.lock().unwrap();
    assert!(
        inner
            .test_run_observations
            .values()
            .all(|row| row.evidence.is_empty())
    );
    drop(inner);
    assert!(
        fixture
            .summaries()
            .iter()
            .all(|run| run.registration.verification_record_ids.is_empty())
    );
}

async fn detail(fixture: &RunFixture, id: &str) -> Value {
    let (status, value): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .uri(format!("/api/test-runs/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

#[tokio::test]
async fn observed_terminal_background_ack_without_locator_is_not_a_running_workload() {
    let fixture = RunFixture::new("observed-terminal-background");
    let session = running_owner(&fixture, "observed-background-runtime");
    let id = start(&fixture, &session, "background-key", LAUNCH);
    let mut events = fixture.state.subscribe_delta_events();
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        "background-key",
        LAUNCH,
        "background task started; no artifact locator",
        Some(EngramCommandExit::NotFinished),
        false,
    );
    let row = fixture.summary(&id);
    assert_eq!(
        row.state,
        TestRunState::Unknown,
        "launch acknowledgement supplies no workload liveness"
    );
    assert!(row.error.as_ref().is_some_and(|reason| !reason.is_empty()));
    assert!(row.ended_at.is_none(), "the workload end time is unknown");
    assert_eq!(detail(&fixture, &id).await["run"]["state"], "unknown");
    let changes = drain_test_run_deltas(&mut events);
    assert!(changes.iter().any(|event| matches!(event,
        DeltaEvent::TestRunChanged { run, .. } if run.run_id == id && run.state == TestRunState::Unknown)));
    assert_no_credit(&fixture);
}

#[test]
fn observed_terminal_legacy_background_marker_is_reconciled_without_runtime_replacement() {
    let fixture = RunFixture::new("observed-terminal-legacy");
    let session = running_owner(&fixture, "observed-legacy-runtime");
    let id = start(&fixture, &session, "legacy-background-key", LAUNCH);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let mut row = inner.test_run_observations.get_mut(&id).unwrap();
        // Persist the old background acknowledgement shape with its runtime
        // still alive. A restart or runtime replacement must not be required.
        row.launch_only = true;
        let row = {
            let snapshot = (*row).clone();
            drop(row);
            snapshot
        };
        fixture
            .state
            .persist_test_command_observation_locked(&mut inner, row);
    }
    fixture.state.step_test_command_observation_persistence();
    let row = fixture.summary(&id);
    assert_eq!(row.state, TestRunState::Unknown);
    assert!(row.error.is_some());
    assert_no_credit(&fixture);
}

async fn terminal_alias_survives_loss(success: bool) {
    let fixture = RunFixture::new("observed-terminal-alias-loss");
    let session = running_owner(&fixture, "observed-alias-runtime");
    let alias = start(&fixture, &session, "alias-key", LAUNCH);
    let (id, directory) = launcher(&fixture, &session);
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        "alias-key",
        LAUNCH,
        &format!("RUN {}", test_run_display_path(&directory)),
        Some(EngramCommandExit::NotFinished),
        false,
    );
    fixture.refresh(&[717]);
    assert_eq!(fixture.summaries().len(), 1);
    assert_eq!(fixture.summary(&id).state, TestRunState::Running);
    finish_artifact(&directory, &id, success);
    let expected = if success {
        TestRunState::Passed
    } else {
        TestRunState::Failed
    };
    let mut events = fixture.state.subscribe_delta_events();
    fixture.refresh(&[]);
    let terminal = fixture.summary(&id);
    assert_eq!(terminal.state, expected);
    assert_eq!(terminal.registration.superseded_run_ids, [alias.clone()]);
    assert!(
        terminal
            .registration
            .tracking_provenances
            .contains(&"hostObserved".to_owned())
    );
    assert_eq!(
        detail(&fixture, &id).await["run"]["state"],
        if success { "passed" } else { "failed" }
    );
    let terminal_changes = drain_test_run_deltas(&mut events);
    assert!(terminal_changes.iter().any(|event| matches!(event,
        DeltaEvent::TestRunChanged { run, .. } if run.run_id == id && run.state == expected)));
    // Only disposable fixture content is removed. A formerly exact terminal
    // observation must not become a phantom live execution when logs vanish.
    fs::remove_dir_all(&fixture.root).unwrap();
    fixture.refresh(&[]);
    let restored = fixture.summary(&alias);
    assert_eq!(
        restored.state, expected,
        "known terminal observation outlives artifacts"
    );
    assert!(restored.ended_at.is_some());
    assert_eq!(restored.exit_code, Some(if success { 0 } else { 101 }));
    assert_eq!(
        detail(&fixture, &alias).await["run"]["state"],
        if success { "passed" } else { "failed" }
    );
    assert!(drain_test_run_deltas(&mut events).iter().all(|event| !matches!(event,
        DeltaEvent::TestRunChanged { run, .. } if run.run_id == alias && run.state == TestRunState::Running)));
    *fixture.state.inner.lock().unwrap() = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture.summary(&alias).state,
        expected,
        "terminal fact survives boot"
    );
    assert_no_credit(&fixture);
}

#[tokio::test]
async fn observed_terminal_exact_pass_does_not_resurrect_after_worktree_loss_or_boot() {
    terminal_alias_survives_loss(true).await;
}

#[tokio::test]
async fn observed_terminal_exact_failure_does_not_resurrect_after_worktree_loss_or_boot() {
    terminal_alias_survives_loss(false).await;
}

#[tokio::test]
async fn observed_terminal_late_provider_end_uses_its_exact_returned_launcher_locator() {
    let fixture = RunFixture::new("observed-terminal-late-result");
    let session = running_owner(&fixture, "observed-late-runtime");
    let alias = start(&fixture, &session, "late-key", LAUNCH);
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        "late-key",
        LAUNCH,
        "background task",
        Some(EngramCommandExit::NotFinished),
        false,
    );
    let (id, directory) = launcher(&fixture, &session);
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    fixture.state.note_engram_reported_end(
        &session,
        "late-key",
        LAUNCH,
        &format!("RUN {}", test_run_display_path(&directory)),
        EngramCommandExit::Code(0),
    );
    fixture.refresh(&[]);
    let runs = fixture.summaries();
    assert_eq!(
        runs.len(),
        1,
        "late output identifies the original execution's exact artifact"
    );
    assert_eq!(runs[0].run_id, id);
    assert_eq!(runs[0].state, TestRunState::Passed);
    assert_eq!(runs[0].registration.superseded_run_ids, [alias]);
    assert_no_credit(&fixture);
}

#[test]
fn observed_terminal_live_native_command_is_not_ended_by_missing_pid_idle_or_missing_path() {
    let fixture = RunFixture::new("observed-terminal-live-native");
    let session = running_owner(&fixture, "observed-live-runtime");
    let id = start(&fixture, &session, "native-live-key", "cargo test");
    fs::remove_dir_all(&fixture.root).unwrap();
    fixture.state.step_test_command_observation_persistence();
    let run = fixture.summary(&id);
    assert_eq!(run.state, TestRunState::Running);
    assert!(run.registration.runner_pid.is_none());
    assert!(run.ended_at.is_none());
    assert_no_credit(&fixture);
}

#[test]
fn observed_terminal_similar_concurrent_launches_are_never_guessed_from_registration() {
    let fixture = RunFixture::new("observed-terminal-concurrent");
    let session = running_owner(&fixture, "observed-concurrent-runtime");
    let first = start(&fixture, &session, "concurrent-one", LAUNCH);
    let second = start(&fixture, &session, "concurrent-two", LAUNCH);
    let (id, directory) = launcher(&fixture, &session);
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert_eq!(fixture.summaries().len(), 3);
    assert_eq!(fixture.summary(&first).state, TestRunState::Running);
    assert_eq!(fixture.summary(&second).state, TestRunState::Running);
    assert!(
        fixture
            .summary(&id)
            .registration
            .superseded_run_ids
            .is_empty()
    );
    assert_no_credit(&fixture);
}

#[test]
fn observed_terminal_native_exit_records_failure_without_verification_credit() {
    let fixture = RunFixture::new("observed-terminal-native-end");
    let session = running_owner(&fixture, "observed-native-end-runtime");
    let id = start(&fixture, &session, "native-end-key", "cargo test");
    fixture.state.observe_test_command_end(
        &session,
        &EngramObservationProvenance::Ambient,
        "native-end-key",
        Some(EngramCommandExit::Code(101)),
    );
    let run = fixture.summary(&id);
    assert_eq!(run.state, TestRunState::Failed);
    assert_eq!(run.exit_code, Some(101));
    assert!(run.ended_at.is_some());
    assert_no_credit(&fixture);
}

#[test]
fn observed_terminal_lost_runtime_without_a_result_is_explicitly_unknown() {
    let fixture = RunFixture::new("observed-terminal-runtime-loss");
    let session = running_owner(&fixture, "observed-lost-runtime");
    let id = start(&fixture, &session, "lost-runtime-key", "cargo test");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::None;
    }
    fixture.state.step_test_command_observation_persistence();
    let run = fixture.summary(&id);
    assert_eq!(run.state, TestRunState::Unknown);
    assert!(run.interrupted);
    assert!(run.error.is_some());
    assert_eq!(run.exit_code, None);
    assert_no_credit(&fixture);
}

async fn background_monitoring_loss(replaced: bool) {
    let fixture = RunFixture::new("observed-terminal-background-monitoring");
    let session = running_owner(&fixture, "observed-background-original-runtime");
    let id = start(&fixture, &session, "background-monitoring-key", LAUNCH);
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        "background-monitoring-key",
        LAUNCH,
        "background task; no artifact locator",
        Some(EngramCommandExit::NotFinished),
        false,
    );
    fixture.state.step_test_command_observation_persistence();
    assert_eq!(fixture.summary(&id).state, TestRunState::Unknown);
    assert!(
        !fixture.summary(&id).interrupted,
        "monitoring is still live"
    );
    let mut events = fixture.state.subscribe_delta_events();
    {
        let runtime = if replaced {
            let (runtime, _commands) =
                test_codex_runtime_handle("observed-background-replacement-runtime");
            SessionRuntime::Codex(runtime)
        } else {
            SessionRuntime::None
        };
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = runtime;
    }
    fixture.state.step_test_command_observation_persistence();
    let summary = fixture.summary(&id);
    assert_eq!(summary.state, TestRunState::Unknown);
    assert!(summary.interrupted);
    assert!(summary.ended_at.is_none(), "workload end remains unknown");
    assert_eq!(summary.exit_code, None);
    assert!(summary.registration.monitoring_interrupted_at.is_some());
    assert!(
        summary
            .error
            .as_deref()
            .unwrap()
            .contains("runtime ended or changed")
    );
    assert!(
        drain_test_run_deltas(&mut events)
            .iter()
            .any(|event| matches!(event,
        DeltaEvent::TestRunChanged { run, .. } if run.run_id == id
            && run.state == TestRunState::Unknown && run.interrupted
            && run.registration.monitoring_interrupted_at.is_some()))
    );
    assert_eq!(detail(&fixture, &id).await["run"]["interrupted"], true);
    let observation = fixture.state.inner.lock().unwrap().test_run_observations[&id].clone();
    assert!(observation.final_unbound_proof.is_none());
    fixture.state.step_test_command_observation_persistence();
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_observations[&id],
        observation
    );
    assert!(
        drain_test_run_deltas(&mut events)
            .iter()
            .all(|event| !matches!(event,
        DeltaEvent::TestRunChanged { run, .. } if run.run_id == id))
    );
    *fixture.state.inner.lock().unwrap() = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let restored = fixture.summary(&id);
    assert_eq!(restored.state, TestRunState::Unknown);
    assert!(restored.interrupted);
    assert_eq!(
        restored.registration.monitoring_interrupted_at,
        summary.registration.monitoring_interrupted_at
    );
    assert_eq!(restored.error, summary.error);
    assert!(restored.ended_at.is_none());
    assert_eq!(restored.exit_code, None);
    assert_no_credit(&fixture);
}

#[tokio::test]
async fn observed_terminal_background_ack_then_runtime_loss_persists_and_publishes_once() {
    background_monitoring_loss(false).await;
}

#[tokio::test]
async fn observed_terminal_background_ack_then_runtime_replacement_persists_and_publishes_once() {
    background_monitoring_loss(true).await;
}
