//! Tests run waits (docs/features/test-runs.md, slice 2 "Run waits"):
//! registration and its validation (root sessions only), the forced rescan,
//! the settle rule in `all` and `any` modes (with a run that left the index,
//! gone or not), the resume prompt and its budgets, the window between
//! picking reads and queuing, Stop, a latched session, a removed session,
//! restart, retention and the wire shape. Liveness is injected so no test
//! depends on a real process's pid.
//!
//! Owns these tests only. New module alongside src/test_run_waits.rs.

use super::*;

struct WaitFixture {
    state: AppState,
    root: PathBuf,
    session: String,
}

impl WaitFixture {
    fn new(label: &str) -> Self {
        let state = test_app_state();
        let root = state
            .test_temp_root
            .as_ref()
            .expect("test root should exist")
            .path()
            .join(format!("test-run-waits-{label}"));
        fs::create_dir_all(&root).unwrap();
        run_git_test_command(&root, &["init", "--quiet"]);
        let project_id = create_test_project(&state, &root, "Waits");
        let session = create_test_project_session(&state, Agent::Claude, &project_id, &root);
        Self {
            state,
            root,
            session,
        }
    }

    fn runs_dir(&self) -> PathBuf {
        self.root.join(".git").join("review-runs")
    }

    fn write(&self, run_id: &str, results: Option<Value>) -> PathBuf {
        let run_dir = self.runs_dir().join(run_id);
        fs::create_dir_all(&run_dir).unwrap();
        let request = json!({ "runId": run_id, "root": self.root.to_string_lossy(),
            "full": true, "started": "2026-09-26T10:00:00.000Z" });
        fs::write(run_dir.join("request.json"), request.to_string()).unwrap();
        if let Some(mut results) = results {
            results["runId"] = json!(run_id);
            fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
        }
        run_dir
    }

    fn scan(&self, alive: &[u32]) {
        let alive = alive.to_vec();
        self.state
            .refresh_test_runs_with(&move |pid, _| alive.contains(&pid), &|event| {
                self.state.publish_delta(event)
            });
        self.state.refresh_test_run_waits();
    }

    fn register(&self, run_ids: &[&str], mode: &str) -> Result<TestRunWaitResponse, ApiError> {
        self.state.create_test_run_wait_with(
            &self.session,
            serde_json::from_value(json!({ "runIds": run_ids, "mode": mode })).unwrap(),
            &|| {},
        )
    }

    fn queued_prompts(&self) -> Vec<String> {
        let inner = self.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&self.session).unwrap()]
            .queued_prompts
            .iter()
            .map(|queued| queued.pending_prompt.text.clone())
            .collect()
    }

    fn pending_waits(&self) -> Vec<TestRunWaitRecord> {
        self.state.inner.lock().unwrap().test_run_waits.clone()
    }

    /// Busy, so a queued resume stays queued where the test can read it.
    fn busy(&self) {
        let mut inner = self.state.inner.lock().unwrap();
        let index = inner.find_session_index(&self.session).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
    }
}

fn running(pid: u32) -> Value {
    json!({ "state": "running", "pid": pid,
        "stages": [{ "name": "rust-tests", "state": "running" }] })
}

fn passed() -> Value {
    json!({ "state": "passed", "exitCode": 0, "ended": "2026-09-26T10:10:00.000Z",
        "stages": [{ "name": "rust-tests", "state": "passed", "code": 0 }] })
}

fn failed(diagnostics: &str) -> Value {
    json!({ "state": "failed", "exitCode": 1, "ended": "2026-09-26T10:10:00.000Z",
        "stages": [{ "name": "rust-tests", "state": "failed", "code": 101,
            "diagnostics": { "text": diagnostics, "truncated": false } }] })
}

#[test]
fn registration_validates_its_runs_and_session() {
    let fixture = WaitFixture::new("validation");
    fixture.write("test-a", Some(running(7)));
    fixture.scan(&[7]);

    let status = |result: Result<TestRunWaitResponse, ApiError>| result.err().map(|error| error.status);
    assert_eq!(status(fixture.register(&[], "all")), Some(StatusCode::BAD_REQUEST));
    let many: Vec<String> = (0..17).map(|index| format!("test-{index}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(status(fixture.register(&many, "all")), Some(StatusCode::BAD_REQUEST));

    // An unknown id gets one forced rescan, then rejects the whole call.
    let rescans = std::cell::Cell::new(0);
    let error = fixture
        .state
        .create_test_run_wait_with(
            &fixture.session,
            serde_json::from_value(json!({ "runIds": ["test-a", "test-missing"] })).unwrap(),
            &|| rescans.set(rescans.get() + 1),
        )
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(rescans.get(), 1);
    assert!(fixture.pending_waits().is_empty());

    // A run in another project is foreign.
    let other_root = fixture.root.with_file_name("test-run-waits-validation-other");
    fs::create_dir_all(&other_root).unwrap();
    run_git_test_command(&other_root, &["init", "--quiet"]);
    let other_project = create_test_project(&fixture.state, &other_root, "Other");
    let elsewhere =
        create_test_project_session(&fixture.state, Agent::Claude, &other_project, &other_root);
    let error = fixture
        .state
        .create_test_run_wait_with(
            &elsewhere,
            serde_json::from_value(json!({ "runIds": ["test-a"] })).unwrap(),
            &|| {},
        )
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("not in the waiting session's project"), "{}", error.message);

    // A session without a project is told so, not that the run is foreign.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&elsewhere).unwrap();
        inner.sessions[index].session.project_id = None;
    }
    let error = fixture
        .state
        .create_test_run_wait_with(
            &elsewhere,
            serde_json::from_value(json!({ "runIds": ["test-a"] })).unwrap(),
            &|| {},
        )
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("has no project"), "{}", error.message);

    // An unknown session.
    let error = fixture
        .state
        .create_test_run_wait_with(
            "session-missing",
            serde_json::from_value(json!({ "runIds": ["test-a"] })).unwrap(),
            &|| {},
        )
        .err()
        .unwrap();
    assert_eq!(error.status, StatusCode::NOT_FOUND);

    // A repeated id is kept once; the wait records its runs as labels.
    let response = fixture.register(&["test-a", "test-a"], "all").unwrap();
    assert_eq!(response.run_ids, vec!["test-a".to_owned()]);
    assert_eq!(response.wait.runs[0].run_id, "test-a");
    assert!(!response.resume_prompt_queued, "still running");
}

#[test]
fn a_run_launched_just_now_is_found_by_the_forced_rescan() {
    let fixture = WaitFixture::new("forced-rescan");
    fixture.scan(&[]);
    fixture.write("test-new", Some(running(7)));
    let response = fixture
        .state
        .create_test_run_wait_with(
            &fixture.session,
            serde_json::from_value(json!({ "runIds": ["test-new"] })).unwrap(),
            &|| {
                fixture
                    .state
                    .refresh_test_runs_with(&|pid, _| pid == 7, &|event| fixture.state.publish_delta(event));
            },
        )
        .unwrap();
    assert_eq!(response.wait_id, response.wait.id);
    assert_eq!(fixture.pending_waits().len(), 1);
}

#[test]
fn an_all_wait_resumes_once_with_every_verdict_when_all_runs_settle() {
    let fixture = WaitFixture::new("all");
    let pass_dir = fixture.write("test-pass", Some(running(7)));
    let fail_dir = fixture.write("test-fail", Some(running(8)));
    fixture.scan(&[7, 8]);
    fixture.busy();
    fixture.register(&["test-pass", "test-fail"], "all").unwrap();

    let mut results = passed();
    results["runId"] = json!("test-pass");
    fs::write(pass_dir.join("results.json"), results.to_string()).unwrap();
    fixture.scan(&[8]);
    assert!(fixture.queued_prompts().is_empty(), "one run still running");
    assert_eq!(fixture.pending_waits().len(), 1);

    let mut results = failed("assertion failed: left == right");
    results["runId"] = json!("test-fail");
    fs::write(fail_dir.join("results.json"), results.to_string()).unwrap();
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "one prompt per wait");
    let prompt = &prompts[0];
    assert!(prompt.contains("### Run `test-pass` (full)"), "{prompt}");
    assert!(prompt.contains("- Verdict: PASS"), "{prompt}");
    assert!(prompt.contains("- Verdict: FAIL"), "{prompt}");
    assert!(prompt.contains("- First failing: stage `rust-tests`"), "{prompt}");
    assert!(prompt.contains("assertion failed: left == right"), "the failure excerpt");
    assert!(prompt.contains("test-launcher.mjs summary"), "the inspect command");
    assert!(!prompt.contains("recover"), "no settle command without UNKNOWN");
    assert!(fixture.pending_waits().is_empty(), "consumed");

    // Never repeated.
    fixture.scan(&[]);
    assert_eq!(fixture.queued_prompts().len(), 1);
}

#[test]
fn an_any_wait_resumes_on_the_first_settled_run() {
    let fixture = WaitFixture::new("any");
    let first = fixture.write("test-first", Some(running(7)));
    fixture.write("test-second", Some(running(8)));
    fixture.scan(&[7, 8]);
    fixture.busy();
    fixture.register(&["test-first", "test-second"], "any").unwrap();
    let mut results = passed();
    results["runId"] = json!("test-first");
    fs::write(first.join("results.json"), results.to_string()).unwrap();
    fixture.scan(&[8]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("`test-second`: NOT SETTLED YET"), "{}", prompts[0]);
}

#[test]
fn only_a_confirmed_unknown_settles_and_it_is_never_a_pass() {
    let fixture = WaitFixture::new("unknown");
    let gone = fixture.write("test-gone", Some(running(7)));
    let _ = gone;
    let unreadable = fixture.write("test-unreadable", Some(running(8)));
    fixture.scan(&[7, 8]);
    fixture.busy();
    fixture.register(&["test-unreadable"], "all").unwrap();
    fixture.register(&["test-gone"], "all").unwrap();

    // Results that cannot be read may still resolve: not settled.
    fs::write(unreadable.join("results.json"), "{ not json").unwrap();
    // The other run's process is gone: settled as unknown.
    fixture.scan(&[]);
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    let prompt = &prompts[0];
    assert!(prompt.contains("`test-gone`: UNKNOWN (process gone)"), "{prompt}");
    assert!(prompt.contains("test-launcher.mjs recover"), "names recover to settle it");
    assert!(prompt.contains("- Stage at the last observation: `rust-tests`"), "{prompt}");
    assert!(!prompt.contains("PASS"), "{prompt}");
    assert_eq!(fixture.pending_waits().len(), 1, "the unreadable run's wait stays");
}

#[test]
fn a_run_that_leaves_the_index_resumes_as_not_indexed_with_its_label() {
    let fixture = WaitFixture::new("not-indexed");
    let run_dir = fixture.write("test-vanishing", Some(running(7)));
    fixture.scan(&[7]);
    fixture.busy();
    let response = fixture.register(&["test-vanishing"], "all").unwrap();
    fs::remove_dir_all(&run_dir).unwrap();
    fixture.scan(&[7]);
    let prompt = fixture.queued_prompts().pop().expect("resumed");
    assert!(prompt.contains("UNKNOWN (not indexed)"), "{prompt}");
    assert!(prompt.contains(&response.wait.runs[0].run_dir), "the label names its evidence");
}

#[test]
fn a_run_dropped_from_the_index_but_still_on_disk_waits_for_the_next_scan() {
    let fixture = WaitFixture::new("dropped");
    let kept = fixture.write("test-kept", Some(running(7)));
    let unreadable = fixture.write("test-unreadable-request", Some(running(8)));
    fixture.scan(&[7, 8]);
    fixture.busy();
    fixture.register(&["test-kept"], "all").unwrap();
    fixture.register(&["test-unreadable-request"], "all").unwrap();
    // Both pass. A rescan that started before the registrations commits
    // without their retention exemption and drops both runs; one of them has
    // also lost its request.json.
    for (run_dir, run_id) in [(&kept, "test-kept"), (&unreadable, "test-unreadable-request")] {
        let mut results = passed();
        results["runId"] = json!(run_id);
        fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    }
    fs::remove_file(unreadable.join("request.json")).unwrap();
    fixture.state.inner.lock().unwrap().test_runs.entries.clear();

    fixture.state.refresh_test_run_waits();
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("`test-unreadable-request`: UNKNOWN (not indexed)"),
        "a run that cannot be indexed again settles: {}",
        prompts[0]
    );
    let pending: Vec<String> = fixture
        .pending_waits()
        .iter()
        .map(|wait| wait.run_ids[0].clone())
        .collect();
    assert_eq!(pending, vec!["test-kept".to_owned()], "a run still on disk is not settled");

    // The next scan, with the wait's exemption, indexes it again.
    fixture.scan(&[]);
    let prompts = fixture.queued_prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].contains("`test-kept`: PASS"), "its real verdict: {}", prompts[1]);
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_run_of_a_removed_project_resumes_as_not_indexed() {
    let fixture = WaitFixture::new("project-removed");
    fixture.write("test-orphaned", Some(running(7)));
    fixture.scan(&[7]);
    fixture.busy();
    fixture.register(&["test-orphaned"], "all").unwrap();
    let project_id = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.session).unwrap()]
            .session
            .project_id
            .clone()
            .unwrap()
    };
    // Its sessions stay, without a project; the index stops scanning the
    // repository, and the run's files are all still there.
    fixture.state.delete_project(&project_id).unwrap();
    fixture.scan(&[7]);
    let prompt = fixture.queued_prompts().pop().expect("resumed, not pending for good");
    assert!(prompt.contains("`test-orphaned`: UNKNOWN (not indexed)"), "{prompt}");
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_preflight_failure_resumes_naming_the_check() {
    let fixture = WaitFixture::new("preflight");
    let run_dir = fixture.write("test-preflight", Some(running(7)));
    fixture.scan(&[7]);
    fixture.busy();
    fixture.register(&["test-preflight"], "all").unwrap();
    let results = json!({ "runId": "test-preflight", "state": "failed", "exitCode": 1,
        "ended": "2026-09-26T10:10:00.000Z", "error": "preflight failed",
        "preflight": [{ "name": "cargo", "code": 127,
            "diagnostics": { "text": "cargo: command not found" } }],
        "stages": [] });
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    fixture.scan(&[]);
    let prompt = fixture.queued_prompts().pop().expect("resumed");
    assert!(prompt.contains("- Verdict: FAIL"), "{prompt}");
    assert!(prompt.contains("- First failing: preflight `cargo`"), "{prompt}");
    assert!(prompt.contains("cargo: command not found"), "{prompt}");
}

#[test]
fn a_run_that_fails_after_the_reads_are_picked_is_read_before_resuming() {
    let fixture = WaitFixture::new("late-failure");
    let run_dir = fixture.write("test-late", Some(running(7)));
    fixture.scan(&[7]);
    fixture.busy();
    fixture.register(&["test-late"], "all").unwrap();
    let mut results = failed("late assertion failed");
    results["runId"] = json!("test-late");
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    // Still running when the reads are picked; failed by the time the resume
    // would be queued.
    fixture.state.refresh_test_run_waits_pausing(&|| {
        fixture
            .state
            .refresh_test_runs_with(&|_, _| false, &|event| fixture.state.publish_delta(event));
    });
    assert!(
        fixture.queued_prompts().is_empty(),
        "deferred, not resumed without diagnostics that are there"
    );
    assert_eq!(fixture.pending_waits().len(), 1);

    fixture.state.refresh_test_run_waits();
    let prompt = fixture.queued_prompts().pop().expect("resumed on the next refresh");
    assert!(prompt.contains("late assertion failed"), "{prompt}");
    assert!(!prompt.contains("not available here"), "{prompt}");
}

#[test]
fn a_latched_session_gets_its_resume_queued_but_not_dispatched() {
    let fixture = WaitFixture::new("latched");
    fixture.write("test-latched", Some(passed()));
    fixture.scan(&[]);
    // As after a Stop: idle, with the explicit-resume latch set.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
    }
    let response = fixture.register(&["test-latched"], "all").unwrap();
    assert!(response.resume_prompt_queued);
    assert!(!response.resume_dispatch_requested, "the latch holds automatic resumes");
    let inner = fixture.state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.orchestrator_auto_dispatch_blocked, "only the user lifts it");
    assert_eq!(record.queued_prompts.len(), 1, "the prompt stays queued");
}

#[test]
fn a_delegation_child_cannot_wait_on_runs() {
    let fixture = WaitFixture::new("child");
    fixture.write("test-a", Some(running(7)));
    fixture.scan(&[7]);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.sessions[index].session.parent_delegation_id = Some("delegation-1".to_owned());
    }
    let error = fixture.register(&["test-a"], "all").err().expect("a child is refused");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("root session"), "{}", error.message);
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn a_wait_on_settled_runs_resumes_at_once() {
    let fixture = WaitFixture::new("settled");
    fixture.write("test-done", Some(passed()));
    fixture.scan(&[]);
    fixture.busy();
    let response = fixture.register(&["test-done"], "all").unwrap();
    assert!(response.resume_prompt_queued);
    assert!(!response.resume_dispatch_requested, "the session is busy");
    assert_eq!(fixture.queued_prompts().len(), 1);
    assert!(fixture.pending_waits().is_empty());
}

#[test]
fn stop_consumes_the_sessions_run_waits_and_nothing_resumes_later() {
    let fixture = WaitFixture::new("stop");
    let run_dir = fixture.write("test-stopped", Some(running(7)));
    fixture.scan(&[7]);
    fixture.busy();
    fixture.register(&["test-stopped"], "all").unwrap();
    let mut deltas = fixture.state.subscribe_delta_events();

    fixture.state.stop_session(&fixture.session).expect("stop should succeed");

    assert!(fixture.pending_waits().is_empty());
    let mut consumed = Vec::new();
    while let Ok(payload) = deltas.try_recv() {
        if let Ok(DeltaEvent::TestRunWaitConsumed { reason, session_id, .. }) =
            serde_json::from_str::<DeltaEvent>(&payload)
        {
            consumed.push((reason, session_id));
        }
    }
    assert_eq!(
        consumed,
        vec![(TestRunWaitConsumedReason::SessionStopped, fixture.session.clone())]
    );
    let mut results = passed();
    results["runId"] = json!("test-stopped");
    fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    fixture.scan(&[]);
    assert!(
        fixture
            .queued_prompts()
            .iter()
            .all(|prompt| !prompt.contains("test-stopped")),
        "no automatic reactivation"
    );
}

#[test]
fn a_removed_session_consumes_its_waits() {
    let fixture = WaitFixture::new("removed");
    fixture.write("test-orphan", Some(running(7)));
    fixture.scan(&[7]);
    fixture.register(&["test-orphan"], "all").unwrap();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.sessions.remove(index);
    }
    let mut deltas = fixture.state.subscribe_delta_events();
    fixture.scan(&[7]);
    assert!(fixture.pending_waits().is_empty());
    let reasons: Vec<TestRunWaitConsumedReason> = std::iter::from_fn(|| deltas.try_recv().ok())
        .filter_map(|payload| match serde_json::from_str::<DeltaEvent>(&payload) {
            Ok(DeltaEvent::TestRunWaitConsumed { reason, .. }) => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(reasons, vec![TestRunWaitConsumedReason::SessionRemoved]);
}

#[test]
fn a_session_that_became_an_archived_codex_thread_has_its_waits_consumed() {
    let fixture = WaitFixture::new("archived");
    fixture.write("test-archived", Some(running(7)));
    fixture.scan(&[7]);
    fixture.register(&["test-archived"], "all").unwrap();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.sessions[index].session.codex_thread_state = Some(CodexThreadState::Archived);
    }
    let mut deltas = fixture.state.subscribe_delta_events();
    fixture.scan(&[7]);
    assert!(fixture.pending_waits().is_empty());
    assert!(fixture.queued_prompts().is_empty(), "consumed, not resumed");
    let reasons: Vec<TestRunWaitConsumedReason> = std::iter::from_fn(|| deltas.try_recv().ok())
        .filter_map(|payload| match serde_json::from_str::<DeltaEvent>(&payload) {
            Ok(DeltaEvent::TestRunWaitConsumed { reason, .. }) => Some(reason),
            _ => None,
        })
        .collect();
    assert_eq!(reasons, vec![TestRunWaitConsumedReason::SessionUnavailable]);
}

#[tokio::test]
async fn the_route_answers_201_with_the_wait_and_422_for_a_malformed_body() {
    let fixture = WaitFixture::new("route");
    fixture.write("test-routed", Some(running(7)));
    fixture.scan(&[7]);
    let app = app_router(fixture.state.clone());
    let post = |body: String| {
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{}/test-run-waits", fixture.session))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    };
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        post(json!({ "runIds": ["test-routed"], "mode": "any" }).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    for key in [
        "waitId",
        "runIds",
        "mode",
        "wait",
        "revision",
        "resumePromptQueued",
        "resumeDispatchRequested",
        "serverInstanceId",
    ] {
        assert!(body.get(key).is_some(), "{key}: {body}");
    }
    assert_eq!(body["runIds"], json!(["test-routed"]));
    assert_eq!(body["mode"], "any");
    assert_eq!(body["wait"]["id"], body["waitId"]);
    assert_eq!(fixture.pending_waits().len(), 1);

    let response = request_response(&app, post("{ not json".to_owned())).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let response = request_response(&app, post(json!({ "runIds": "test-routed" }).to_string())).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fixture.pending_waits().len(), 1, "nothing registered by a rejected body");
}

#[test]
fn nothing_settles_before_the_first_scan_of_a_process() {
    let fixture = WaitFixture::new("first-scan");
    fixture.write("test-before-boot", Some(running(7)));
    fixture.scan(&[7]);
    fixture.busy();
    fixture.register(&["test-before-boot"], "all").unwrap();
    // As after a restart: the wait is loaded, the index is still empty.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner.test_runs = TestRunIndex::default();
    }
    fixture.state.refresh_test_run_waits();
    assert_eq!(fixture.pending_waits().len(), 1, "an empty index proves nothing");
    fixture.scan(&[7]);
    assert_eq!(fixture.pending_waits().len(), 1, "still running after the scan");
}

#[test]
fn pending_waits_are_persisted_and_reload() {
    let fixture = WaitFixture::new("persisted");
    fixture.write("test-kept", Some(running(7)));
    fixture.scan(&[7]);
    let response = fixture.register(&["test-kept"], "all").unwrap();
    let reloaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.test_run_waits, vec![response.wait]);
    assert!(!reloaded.test_runs.scanned_once);
}

#[test]
fn a_run_named_by_a_pending_wait_stays_indexed_past_retention() {
    let fixture = WaitFixture::new("retention");
    let old = fixture.write("test-a-old", Some(running(7)));
    fixture.scan(&[7]);
    fixture.register(&["test-a-old"], "all").unwrap();
    // It finishes behind 50 newer terminal runs.
    let mut results = passed();
    results["runId"] = json!("test-a-old");
    fs::write(old.join("results.json"), results.to_string()).unwrap();
    fs::write(
        old.join("request.json"),
        json!({ "runId": "test-a-old", "root": fixture.root.to_string_lossy(),
                "full": true, "started": "2026-09-20T10:00:00.000Z" })
        .to_string(),
    )
    .unwrap();
    for index in 0..TEST_RUN_TERMINAL_RUNS_PER_PROJECT {
        fixture.write(&format!("test-newer-{index:02}"), Some(passed()));
    }
    fixture.busy();
    fixture.state.refresh_test_runs_with(&|_, _| false, &|event| fixture.state.publish_delta(event));
    assert!(
        fixture.state.test_run_summaries(None).iter().any(|run| run.run_id == "test-a-old"),
        "held for its wait"
    );
    fixture.state.refresh_test_run_waits();
    let prompt = fixture.queued_prompts().pop().expect("resumed");
    assert!(prompt.contains("`test-a-old`: PASS"), "with its verdict, not as not indexed");
}

/// A failed run as the index summarizes it, for prompt tests.
fn failed_summary(run_id: &str) -> TestRunSummary {
    TestRunSummary {
        run_id: run_id.to_owned(),
        project_id: Some("project-1".to_owned()),
        worktree: "/repo".to_owned(),
        run_dir: format!("/repo/.git/review-runs/{run_id}"),
        preset: TestRunPreset::Full,
        command: None,
        command_truncated: false,
        detached: Some(true),
        state: TestRunState::Failed,
        unknown_reason: None,
        interrupted: false,
        current_stage: None,
        stages: vec![TestRunStageSummary {
            name: "rust-tests".to_owned(),
            state: TestRunStageState::Failed,
            exit_code: Some(101),
            started_at: None,
            ended_at: None,
        }],
        owner_session_id: None,
        notify_to: None,
        notify_session_id: None,
        started_at: None,
        ended_at: None,
        exit_code: Some(1),
        error: None,
        detail_version: None,
    }
}

/// An `all` wait on these runs, labelled from their summaries.
fn wait_on(summaries: &[TestRunSummary]) -> TestRunWaitRecord {
    TestRunWaitRecord {
        id: "test-run-wait-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_ids: summaries.iter().map(|summary| summary.run_id.clone()).collect(),
        mode: DelegationWaitMode::All,
        created_at: "now".to_owned(),
        title: None,
        runs: summaries
            .iter()
            .map(|summary| TestRunWaitRunLabel {
                run_id: summary.run_id.clone(),
                run_dir: summary.run_dir.clone(),
                preset: summary.preset,
                worktree: summary.worktree.clone(),
                owner_session_id: None,
                started_at: None,
            })
            .collect(),
    }
}

/// A read that found the first failure, with this excerpt.
fn failure_read(excerpt: String, truncated: bool) -> TestRunCardFailureRead {
    TestRunCardFailureRead::Read {
        digest: "d".to_owned(),
        failure: Some(TestRunCardFailure {
            phase: TestRunCardFailurePhase::Stage,
            name: "rust-tests".to_owned(),
            excerpt,
            truncated,
        }),
    }
}

/// The resume prompt of a wait on these failed runs, with these reads.
fn failed_prompt(
    summaries: &[TestRunSummary],
    failures: &HashMap<String, TestRunCardFailureRead>,
) -> String {
    let wait = wait_on(summaries);
    let refs: Vec<Option<&TestRunSummary>> = summaries.iter().map(Some).collect();
    let verdicts = test_run_wait_verdicts(&wait, &refs, |_| false);
    build_test_run_wait_prompt(&wait, &refs, &verdicts, failures)
}

#[test]
fn the_prompt_keeps_headers_whole_and_bounds_excerpts() {
    let run_ids: Vec<String> = (0..16).map(|index| format!("test-{index:02}")).collect();
    let summaries: Vec<TestRunSummary> = run_ids.iter().map(|id| failed_summary(id)).collect();
    let failures: HashMap<String, TestRunCardFailureRead> = run_ids
        .iter()
        .map(|id| (id.clone(), failure_read("x".repeat(TEST_RUN_WAIT_EXCERPT_MAX_BYTES), false)))
        .collect();
    let prompt = failed_prompt(&summaries, &failures);
    assert!(prompt.len() <= TEST_RUN_WAIT_PROMPT_MAX_BYTES, "{}", prompt.len());
    assert!(!prompt.contains(DELEGATION_WAIT_RESUME_TRUNCATED_MARKER), "budgeted, not truncated");
    for id in &run_ids {
        assert!(prompt.contains(&format!("### Run `{id}` (full)")), "every header whole");
        assert!(prompt.contains(&format!("summary \"/repo/.git/review-runs/{id}\"")));
    }
    assert!(prompt.contains("Diagnostics (first failure) (cut)"), "excerpts share the rest");

    // A single run gets at most 8 KiB of excerpt.
    let prompt = failed_prompt(&summaries[..1], &failures);
    assert!(prompt.len() < TEST_RUN_WAIT_EXCERPT_MAX_BYTES + 2048, "{}", prompt.len());
}

#[test]
fn an_excerpt_cut_before_the_prompt_is_marked_and_its_fences_cannot_close_the_block() {
    let summaries = vec![failed_summary("test-a"), failed_summary("test-b")];
    let failures = HashMap::from([
        // Short enough for the prompt, but the launcher capped it.
        ("test-a".to_owned(), failure_read("partial output".to_owned(), true)),
        // Test output that contains a fence of its own.
        ("test-b".to_owned(), failure_read("expected:\n```\nleft\n```".to_owned(), false)),
    ]);
    let prompt = failed_prompt(&summaries, &failures);
    let (first, second) = prompt.split_once("### Run `test-b`").expect("both runs");
    assert!(
        first.contains("Diagnostics (first failure) (cut):\n```\npartial output\n```"),
        "{first}"
    );
    assert!(
        second.contains("Diagnostics (first failure):\n````\nexpected:\n```\nleft\n```\n````"),
        "{second}"
    );
}

#[test]
fn a_label_run_dir_names_its_directory() {
    let display = |path: &str| test_run_display_path(FsPath::new(path));
    assert_eq!(
        test_run_fs_path(&display("/repo/.git/review-runs/test-a")),
        PathBuf::from("/repo/.git/review-runs/test-a")
    );
    #[cfg(windows)]
    {
        assert_eq!(
            test_run_fs_path(&display(r"\\?\C:\repo\.git\review-runs\test-a")),
            PathBuf::from("C:/repo/.git/review-runs/test-a")
        );
        // Displayed as `UNC/server/...`, which alone would be relative.
        assert_eq!(
            test_run_fs_path(&display(r"\\?\UNC\server\share\repo\.git\review-runs\test-a")),
            PathBuf::from(r"\\?\UNC\server\share\repo\.git\review-runs\test-a")
        );
    }
}

#[test]
fn the_wait_and_its_events_have_the_contract_wire_shape() {
    let wait = TestRunWaitRecord {
        id: "test-run-wait-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_ids: vec!["test-a".to_owned()],
        mode: DelegationWaitMode::All,
        created_at: "now".to_owned(),
        title: None,
        runs: vec![TestRunWaitRunLabel {
            run_id: "test-a".to_owned(),
            run_dir: "/repo/.git/review-runs/test-a".to_owned(),
            preset: TestRunPreset::Focused,
            worktree: "/repo".to_owned(),
            owner_session_id: None,
            started_at: None,
        }],
    };
    let value = serde_json::to_value(&wait).unwrap();
    for key in ["id", "sessionId", "runIds", "mode", "createdAt", "runs"] {
        assert!(value.get(key).is_some(), "{key}: {value}");
    }
    assert_eq!(value["mode"], "all");
    assert!(value.get("title").is_none());
    for key in ["runId", "runDir", "preset", "worktree", "ownerSessionId", "startedAt"] {
        assert!(value["runs"][0].get(key).is_some(), "{key}");
    }
    let consumed = serde_json::to_value(DeltaEvent::TestRunWaitConsumed {
        revision: 3,
        wait_id: "test-run-wait-1".to_owned(),
        session_id: "session-1".to_owned(),
        reason: TestRunWaitConsumedReason::SessionUnavailable,
    })
    .unwrap();
    assert_eq!(consumed["type"], "testRunWaitConsumed");
    assert_eq!(consumed["reason"], "sessionUnavailable");
    assert_eq!(consumed["sessionId"], "session-1");
    let created = serde_json::to_value(DeltaEvent::TestRunWaitCreated { revision: 2, wait }).unwrap();
    assert_eq!(created["type"], "testRunWaitCreated");
    let failed = serde_json::to_value(DeltaEvent::TestRunWaitResumeDispatchFailed {
        revision: 4,
        session_id: "session-1".to_owned(),
        error: "x".to_owned(),
    })
    .unwrap();
    assert_eq!(failed["type"], "testRunWaitResumeDispatchFailed");
}
