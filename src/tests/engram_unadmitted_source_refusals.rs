//! Gitlink source refusals preserve outcome/check reporting without source
//! attribution. Other permanent refusals retire closing custody once, while
//! transient absence keeps the existing retry path and local test verdicts.
use super::*;
use super::super::super::declared_checks::{
    declare, write_artifact, DECLARED_ARTIFACT, DECLARED_TEST, FAILING_TRX,
};

/// Feed real Claude declared-command start/completion telemetry with reported
/// success and a failing artifact, so source settlement cannot hide a verdict
/// regression by merely preserving shell success. Claude reports no numeric
/// exit code for this successful Bash result.
pub(super) fn observe_declared_failure(fixture: &mut ReportingTurn) -> Vec<TestRunSummary> {
    declare(&fixture.turn, DECLARED_TEST, DECLARED_ARTIFACT);
    fixture.reader.feed(json!({"type": "assistant", "message": {"content": [{
        "type": "tool_use", "id": "source-refusal-check", "name": "Bash",
        "input": {"command": DECLARED_TEST}
    }]}}));
    write_artifact(&fixture.turn, FAILING_TRX);
    fixture.reader.feed(json!({"type": "user", "message": {
        "role": "user", "content": [{"type": "tool_result",
            "tool_use_id": "source-refusal-check", "is_error": false, "content": "process exited zero"}]
    }, "tool_use_result": {"stdout": "process exited zero", "stderr": "", "interrupted": false}}));
    fixture.turn.wait_for_snapshots();
    let runs = fixture.turn.state.test_run_summaries(None);
    assert_eq!(runs.len(), 1, "a real local observed test row must exist");
    assert_eq!(runs[0].state, TestRunState::Failed, "the artifact decides the verdict");
    assert_eq!(runs[0].exit_code, None, "reported success must not invent a numeric exit");
    assert!(runs[0].registration.verification_record_ids.is_empty());
    runs
}

pub(super) fn assert_declared_failure_unchanged(
    fixture: &ReportingTurn,
    before: &[TestRunSummary],
) {
    assert_eq!(fixture.turn.state.test_run_summaries(None).as_slice(), before);
    assert_eq!(fs::read(fixture.turn.root.join(DECLARED_ARTIFACT)).unwrap().as_slice(), FAILING_TRX);
}

fn permanent_source_refusal_settles_once(
    label: &str,
    configure: impl FnOnce(&FsPath),
    reason: &str,
    move_scope: bool,
) {
    let mut fixture = ReportingTurn::new(label, true, ReplyMode::Recorded, false);
    let local_runs = observe_declared_failure(&mut fixture);
    configure(&fixture.turn.root);

    // Exercise the production bounded thread path too. The functional RPC
    // fixture below has a separate scripted capture seam so its 250ms RPC
    // deadline never becomes an assertion about local Git scheduling.
    let capture = fixture.turn.state.inner.lock().unwrap()
        .test_engram_source_capture.take().unwrap();
    let root = fixture.turn.root.to_string_lossy().into_owned();
    let common = engram_source_root_common_dir(&fixture.turn.root).unwrap();
    for place in [
        EngramBasisPlace::Workdir(root.clone()),
        EngramBasisPlace::Named { root, common_dir_key: engram_exact_path_key(&common) },
    ] {
        let refusal = fixture.turn.state.engram_source_capture_within(&place, DEADLOCK_GUARD)
            .expect_err("the bounded capture must preserve the definitive cause");
        assert!(refusal.to_string().contains(reason));
    }
    fixture.turn.state.inner.lock().unwrap().test_engram_source_capture = Some(capture);

    // Count the real fixture capture, not a synthetic refusal. The fixture
    // clock keeps unrelated RPC deadlines independent of local Git latency.
    let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let mut inner = fixture.turn.state.inner.lock().unwrap();
        let capture = inner.test_engram_source_capture.clone().unwrap();
        let count = captures.clone();
        let shared = Arc::downgrade(&fixture.turn.state.inner);
        let session_id = fixture.turn.session_id.clone();
        let successor = fixture.turn.root.join("successor").to_string_lossy().into_owned();
        inner.test_engram_source_capture = Some(Arc::new(move |place, budget| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let result = capture(place, budget);
            if move_scope {
                assert!(result.is_err(), "the old workspace really refused capture");
                let shared = shared.upgrade().unwrap();
                let mut inner = shared.lock().unwrap();
                inner.session_mut(&session_id).unwrap().session.workdir = successor.clone();
            }
            result
        }));
    }
    let (events, received) = mpsc::channel();
    *fixture.turn.state.runtime_observation_lifetime.install_hook.lock().unwrap() =
        Some(Arc::new(move |phase| {
            if matches!(phase, "closing_installed" | "idle") {
                let _ = events.send(phase);
            }
        }));
    fixture.finish();
    let job = loop {
        crate::tests::phase_sync::receive(&received, "the first closing capture settles");
        let job = fixture.turn.record(|record| record.engram.unadmitted_closing_jobs.first().cloned());
        if let Some(job) = job.filter(|job| job.attempts > 0) {
            break job;
        }
    };
    assert_eq!(job.attempts, 1);
    let refusal = job.terminal_failure.as_ref()
        .expect("a definitive source-basis refusal must settle on its first capture");
    let expected_reason = if move_scope { "scope changed" } else { reason };
    assert!(refusal.contains(expected_reason), "the current cause is retained: {refusal}");
    if move_scope {
        assert!(!refusal.contains(reason), "a stale workspace refusal must not be installed");
    }
    assert_eq!(job.last_error.as_ref(), Some(refusal));
    assert_eq!(fixture.turn.record(|record| record.engram.unadmitted_reports.clone()),
        fixture.setup_reports, "a refused source must not invent an observation");
    assert!(fixture.producer.requests().is_empty());

    while fixture.turn.record(|record| record.engram.unadmitted_worker_running) {
        crate::tests::phase_sync::receive(&received, "the settled closing worker becomes idle");
    }
    for _ in 0..3 {
        fixture.turn.state.start_runtime_observation_worker(&fixture.turn.session_id);
        assert!(!fixture.turn.record(|record| record.engram.unadmitted_worker_running));
    }
    assert_eq!(captures.load(std::sync::atomic::Ordering::SeqCst), 1,
        "later worker kicks must not retry a definitive refusal");

    let notices = fixture.turn.record(system_notices);
    let matching: Vec<_> = notices.iter().filter(|text| text.contains(&job.trace.host_turn_ref)
        && text.contains("not reported to Engram")).collect();
    assert_eq!(matching.len(), 1, "one visible refusal: {notices:?}");
    assert!(matching[0].contains(if move_scope { "source scope changed" } else { "source capture was refused" }));
    assert!(!matching[0].contains(refusal), "the retained cause is not copied into the notice");
    let durable = super::repairs::stored_session(&fixture.turn.state, &fixture.turn.session_id);
    let restored = serde_json::from_str::<PersistedSessionRecord>(&durable).unwrap()
        .into_record().unwrap();
    assert_eq!(restored.engram.unadmitted_closing_jobs[0], job,
        "retirement and its original cause survive session serialization");
    assert_declared_failure_unchanged(&fixture, &local_runs);
    fixture.assert_no_granted_reporting();
    retire_reporting_hosts(fixture, Vec::new());
}

#[test]
fn closing_source_refusal_configured_filter_settles_once() {
    permanent_source_refusal_settles_once("closing-refused-filter", |root| {
        run_git_test_command(root, &["config", "filter.lfs.process", "must-not-run-a-filter"]);
    }, "filters", false);
}

fn add_gitlink(root: &FsPath) {
    let head = run_git_test_command_output(root, &["rev-parse", "HEAD"]);
    run_git_test_command(root, &["update-index", "--add", "--cacheinfo", "160000", head.trim(), "child"]);
    assert!(matches!(engram_execution_source_capture_with(root, false),
        Err(SourceBasisRefusal::UnsupportedGitlinks)), "the real content capture must refuse gitlinks");
}

fn settle_closing(fixture: &ReportingTurn) {
    let guard = crate::tests::phase_sync::PollGuard::new();
    loop {
        let settled = fixture.turn.record(|record| {
            record.engram.unadmitted_active_turn.is_none()
                && !record.engram.unadmitted_worker_running
                && record.engram.unadmitted_closing_jobs.iter().all(|job| job.terminal_failure.is_some())
                && record.engram.unadmitted_reports.iter().all(EngramRuntimeObservationReport::is_settled)
        });
        if settled { return; }
        guard.wait("the closing worker settles either its report or its explicit refusal");
    }
}

#[test]
fn closing_gitlink_reports_declared_verdict_without_source_or_credit() {
    let mut fixture = ReportingTurn::new("closing-gitlink-audit", true, ReplyMode::Recorded, false);
    add_gitlink(&fixture.turn.root);
    let local_runs = observe_declared_failure(&mut fixture);
    let check = fixture.turn.record(|record|
        record.engram.unadmitted_active_turn.as_ref().unwrap().commands[0].check.clone());
    assert!(check.source_basis.is_none(), "an unmeasured check has no revision");
    fixture.turn.state.shutdown_persist_blocking();
    fixture.finish();
    settle_closing(&fixture);
    let requests = fixture.producer.requests();
    assert_eq!(requests.len(), 1, "a gitlink source refusal must not discard the outcome/check report");
    let sent = fixture.observation();
    assert!(sent.durable_before_send);
    let body = occurrence(&sent);
    assert_eq!(body["kind"], "unadmitted_turn");
    assert_eq!(body["outcome"], "unknown");
    assert!(body["source_change"].is_null(), "unreported source is never assumed changed or unchanged");
    assert_eq!(body["observed_checks"], json!([check]));
    assert_eq!(body["observed_checks"][0]["observed_result"], "failed");
    assert_eq!(sent.receipt["accounting"]["kind"], "no_source_change");
    assert_eq!(sent.receipt["observed_checks"][0]["credit"], "uncredited");
    assert!(fixture.turn.record(|record| record.engram.runtime_checkpoint.is_none()));
    assert!(fixture.turn.record(|record| record.engram.unadmitted_closing_jobs.is_empty()));
    let notices = fixture.turn.record(system_notices);
    assert_eq!(notices.iter().filter(|line| line.contains("has no measured source")
        && line.contains("submodules")).count(), 1, "the source limitation stays visible: {notices:?}");
    let connection = open_sqlite_state_read_connection(&fixture.turn.state.persistence_path).unwrap();
    let durable = load_session_records_from_sqlite(&connection, &fixture.turn.state.persistence_path)
        .unwrap().into_iter().find(|record| record.session.id == fixture.turn.session_id)
        .expect("the stored session includes its normalized transcript");
    // Release the test's own SQLite handle before retiring the last host;
    // Windows cannot remove its fixture directory while this reader is open.
    drop(connection);
    let restored = durable.into_record().unwrap();
    assert_eq!(system_notices(&restored), notices, "the diagnostic is durable with its report");
    assert_eq!(restored.engram.unadmitted_reports,
        fixture.turn.record(|record| record.engram.unadmitted_reports.clone()));
    assert!(restored.engram.unadmitted_closing_jobs.is_empty());
    assert!(restored.engram.runtime_checkpoint.is_none());
    assert_declared_failure_unchanged(&fixture, &local_runs);
    fixture.assert_no_granted_reporting();
    retire_reporting_hosts(fixture, Vec::new());
}

#[test]
fn closing_gitlink_gap_cannot_reuse_an_older_measured_baseline() {
    let mut fixture = ReportingTurn::new("closing-gitlink-gap", true, ReplyMode::Recorded, false);
    let before = fixture.turn.record(|record|
        record.engram.runtime_checkpoint.as_ref().unwrap().endpoint.clone());
    add_gitlink(&fixture.turn.root);
    fixture.finish();
    settle_closing(&fixture);
    assert_eq!(fixture.producer.requests().len(), 1,
        "the unmeasured turn must publish an outcome-only observation");
    let unmeasured = fixture.observation();
    assert!(occurrence(&unmeasured)["source_change"].is_null());
    assert!(fixture.turn.record(|record| record.engram.runtime_checkpoint.is_none()),
        "an outcome-only receipt cannot establish or retain a checkpoint");

    // Restore the original measurable tree. Equal endpoints must not turn
    // the intervening unmeasured submodule state into measured continuity.
    run_git_test_command(&fixture.turn.root, &["update-index", "--force-remove", "child"]);
    assert_eq!(fixture.turn.revision(), before.source_revision);
    fixture.reader = ClaudeReaderOn::new(&fixture.turn, new_claude_turn_ownership());
    for frame in [task_notification(), init(), assistant_text("Inspecting after the source gap.")] {
        fixture.reader.feed(frame);
    }
    fixture.finish();
    settle_closing(&fixture);
    let measured = fixture.observation();
    assert_eq!(occurrence(&measured)["source_change"]["detection"], "assumed_missing_baseline",
        "the later measurable close must not compare against the pre-gap endpoint");
    assert_eq!(occurrence(&measured)["source_change"]["sighting"]["source_basis"]["source_revision"],
        before.source_revision);
    let sighting = fixture.observation();
    assert_eq!(occurrence(&sighting)["kind"], "source_sighting");
    assert_eq!(fixture.producer.requests().len(), 3,
        "only the later measured close establishes a real source sighting");
    fixture.assert_no_granted_reporting();
    retire_reporting_hosts(fixture, Vec::new());
}

#[test]
fn closing_source_refusal_after_scope_moves_keeps_scope_changed_reason() {
    permanent_source_refusal_settles_once("closing-refused-moved-scope", |root| {
        run_git_test_command(root, &["config", "filter.lfs.process", "must-not-run-a-filter"]);
    }, "filters", true);
}

#[test]
fn closing_gitlink_after_scope_moves_cannot_publish_source_free_facts() {
    permanent_source_refusal_settles_once("closing-gitlink-moved-scope", add_gitlink, "submodules", true);
}

#[test]
fn closing_missing_workdir_does_not_inherit_enclosing_repository_refusal() {
    let fixture = ReportingTurn::new("closing-missing-nested-workdir", true, ReplyMode::Recorded, false);
    let head = run_git_test_command_output(&fixture.turn.root, &["rev-parse", "HEAD"]);
    run_git_test_command(&fixture.turn.root,
        &["update-index", "--add", "--cacheinfo", "160000", head.trim(), "child"]);
    let missing = fixture.turn.root.join("absent-worktree");
    assert!(!missing.exists());
    assert!(test_runs_git_common_dir(&missing).is_none(),
        "source naming must validate the session directory before ancestor normalization");
    assert!(engram_execution_source_capture_with(&fixture.turn.root, false).is_err(),
        "the enclosing repository really refuses capture");
    assert!(matches!(engram_execution_source_capture_with(&missing, false), Ok(None)),
        "transient workdir absence must not become a permanent refusal");

    // Once the nested workspace exists, it is measured on its own and a
    // normal subdirectory shares that workspace's source, not its ancestor's.
    fs::create_dir(&missing).unwrap();
    run_git_test_command(&missing, &["init", "--quiet"]);
    let nested = engram_execution_source_capture_with(&missing, false).unwrap().unwrap();
    assert_eq!(nested.workspace_id, fs::canonicalize(&missing).unwrap().to_string_lossy());
    let subdirectory = missing.join("src");
    fs::create_dir(&subdirectory).unwrap();
    assert_eq!(engram_execution_source_capture_with(&subdirectory, false).unwrap(), Some(nested));
    retire_reporting_hosts(fixture, Vec::new());
}
