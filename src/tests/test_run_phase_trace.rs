// Owns timing-ring bounds, exact query selection, and target containment tests.
// Does not run a launcher or treat diagnostic observations as verification.
// Split from test_run_registration_fence.rs beside its real-fence tests.
use super::*;

#[test]
fn registration_target_accepts_only_the_exact_canonical_git_parent_chains() {
    let root = TestTempRoot::create("registration-target");
    let common = root.join("git");
    for suffix in [
        "review-runs/test-main",
        "worktrees/one/review-runs/test-linked",
        "worktrees/one/extra/review-runs/test-nested",
        "other/review-runs/test-other",
    ] {
        fs::create_dir_all(common.join(suffix)).unwrap();
    }
    for (suffix, expected) in [
        ("review-runs/test-main", true),
        ("worktrees/one/review-runs/test-linked", true),
        ("worktrees/one/extra/review-runs/test-nested", false),
        ("other/review-runs/test-other", false),
    ] {
        assert_eq!(
            test_run_registration_target(&common, &fs::canonicalize(common.join(suffix)).unwrap()),
            expected,
            "{suffix}"
        );
    }
    let outside = root.join("outside");
    fs::create_dir_all(outside.join("test-escaped")).unwrap();
    // mklink parses forward slashes as switches; build its path natively.
    let redirect = common.join("worktrees").join("redirect");
    fs::create_dir_all(&redirect).unwrap();
    crate::tests::engram_child_temp::link_directory(&redirect.join("review-runs"), &outside);
    assert!(!test_run_registration_target(
        &common,
        &fs::canonicalize(redirect.join("review-runs/test-escaped")).unwrap()
    ));
}

#[test]
fn phase_ring_is_bounded_and_evicted_inflight_owners_cannot_recreate_rows() {
    let ring = TestRunPhaseTraces::default();
    let old = ring.begin(
        "registration",
        Some("test-old"),
        "old",
        std::time::Instant::now(),
    );
    for _ in 0..TEST_RUN_TRACE_CAPACITY {
        drop(ring.begin(
            "registration",
            Some("test-current"),
            "current",
            std::time::Instant::now(),
        ));
    }
    old.mark("late");
    drop(old);
    assert_eq!(
        ring.0.lock().unwrap().entries.len(),
        TEST_RUN_TRACE_CAPACITY
    );
    assert!(ring.selected(Some("test-old"), None).is_empty());
    let current = ring.selected(Some("test-current"), Some("current"));
    assert_eq!(current.len(), TEST_RUN_TRACE_CAPACITY);
    assert!(current.iter().all(|trace| trace.completed));
}

#[test]
fn phase_ring_bounds_phases_and_keeps_monotonic_host_relative_points() {
    let ring = TestRunPhaseTraces::default();
    let trace = ring.begin(
        "registration",
        Some("test-phases"),
        "phases",
        std::time::Instant::now(),
    );
    for _ in 0..TEST_RUN_TRACE_PHASES + 5 {
        trace.mark("phase");
    }
    drop(trace);
    let selected = ring.selected(None, Some("phases"));
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].phases.len(), TEST_RUN_TRACE_PHASES);
    assert!(selected[0].truncated);
    assert!(selected[0].completed);
    assert!(
        selected[0]
            .phases
            .windows(2)
            .all(|pair| pair[0].monotonic_micros <= pair[1].monotonic_micros)
    );
}

#[test]
fn phase_selection_retains_overlapping_scans_without_unrelated_registrations() {
    let ring = TestRunPhaseTraces::default();
    let scan = ring.begin("rescan", None, "scan", std::time::Instant::now());
    let run = ring.begin(
        "registration",
        Some("test-one"),
        "one",
        std::time::Instant::now(),
    );
    drop(ring.begin(
        "registration",
        Some("test-two"),
        "two",
        std::time::Instant::now(),
    ));
    drop(run);
    drop(scan);
    let selected = ring.selected(Some("test-one"), Some("one"));
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().any(|trace| trace.operation_id == "scan"));
    assert!(!selected.iter().any(|trace| trace.operation_id == "two"));
}

#[tokio::test]
async fn diagnostic_queries_are_exact_bounded_and_do_not_persist_state() {
    let state = test_app_state();
    drop(state.test_run_phase_traces.begin(
        "registration",
        Some("test-query"),
        "operation-query",
        std::time::Instant::now(),
    ));
    let app = app_router(state.clone());
    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri("/api/test-run-diagnostics?runId=test-query")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["capacity"], TEST_RUN_TRACE_CAPACITY);
    assert_eq!(response["traces"][0]["operationId"], "operation-query");
    assert_eq!(response["serverInstanceId"], state.server_instance_id);
    for uri in [
        "/api/test-run-diagnostics",
        "/api/test-run-diagnostics?runId=bad%2Fpath",
    ] {
        let (status, _): (StatusCode, Value) = request_json(
            &app,
            Request::builder().uri(uri).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .is_empty()
    );
}

#[test]
fn registration_trace_survives_cancellation_of_the_actual_http_reply() {
    let mut fixture = FenceFixture::new();
    let mut state = test_app_state();
    let root = fixture.root.path().join("cancelled-registration");
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project = create_test_project(&state, &root, "Cancelled reply");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    let run_id = "test-cancelled-registration";
    let directory = root.join(".git/review-runs").join(run_id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("request.json"),
        json!({
            "runId":run_id,"root":test_run_display_path(&root),"owner":session,
            "full":true,"stages":[{"name":"rust-tests"}],
            "started":engram_command_started_at(),"creatorPid":std::process::id()
        })
        .to_string(),
    )
    .unwrap();
    state.persist_tx = fixture.tx.clone();
    state.persistence_path = Arc::new(fixture.root.path().join("termal.sqlite"));
    fixture.inner = state.inner.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let app = app_router(state.clone());
    let request = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{session}/test-runs"))
        .header("content-type", "application/json")
        .header("x-termal-registration-operation", "cancelled-operation")
        .body(Body::from(
            json!({"runId":run_id,"runDirectory":test_run_display_path(&directory)}).to_string(),
        ))
        .unwrap();
    let caller = runtime.spawn(async move { app.oneshot(request).await });
    // The actual handler has queued its fence, but SQLite has not committed.
    fixture.receive(false);
    let fence = fixture.rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    assert!(matches!(&fence, PersistRequest::Fence(_)));
    fixture.batch.accept(fence);
    caller.abort();
    assert!(runtime.block_on(caller).unwrap_err().is_cancelled());
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    // Tokio joins the blocking handler even after its HTTP waiter is gone.
    runtime.shutdown_timeout(phase_sync::DEADLOCK_GUARD);
    let selected = state
        .test_run_phase_traces
        .selected(Some(run_id), Some("cancelled-operation"));
    assert_eq!(selected.len(), 1);
    assert!(selected[0].completed);
    for phase in [
        "received",
        "validated",
        "durable",
        "response_ready",
        "finished",
    ] {
        assert!(
            selected[0].phases.iter().any(|point| point.phase == phase),
            "{phase}"
        );
    }
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .registered
            .contains(run_id)
    );
}

#[test]
fn registration_refuses_a_request_changed_while_its_durability_fence_waited() {
    let mut fixture = FenceFixture::new();
    let mut state = test_app_state();
    let root = fixture.root.path().join("changed-registration");
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project = create_test_project(&state, &root, "Changed request");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    let run_id = "test-changed-registration";
    let directory = root.join(".git/review-runs").join(run_id);
    fs::create_dir_all(&directory).unwrap();
    let mut request = json!({
        "runId":run_id,"root":test_run_display_path(&root),"owner":session,
        "full":true,"stages":[{"name":"rust-tests"}],
        "started":engram_command_started_at(),"creatorPid":std::process::id()
    });
    fs::write(directory.join("request.json"), request.to_string()).unwrap();
    state.persist_tx = fixture.tx.clone();
    state.persistence_path = Arc::new(fixture.root.path().join("termal.sqlite"));
    fixture.inner = state.inner.clone();
    let (queued_tx, queued_rx) = mpsc::channel();
    let worker = {
        let state = state.clone();
        let session = session.clone();
        let directory = directory.clone();
        std::thread::spawn(move || {
            TEST_TEST_RUN_REGISTRATION_FENCE_SENT.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || queued_tx.send(()).unwrap()));
            });
            state.register_test_run(
                &session,
                RegisterTestRunRequest {
                    run_id: run_id.to_owned(),
                    run_directory: test_run_display_path(&directory),
                },
            )
        })
    };
    let written = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        queued_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        fixture.receive(false);
        assert!(!fixture.batch.drain(&fixture.rx, false));
        let delta = fixture.collect();
        request["creatorPid"] = json!(std::process::id() + 1);
        fs::write(directory.join("request.json"), request.to_string()).unwrap();
        fixture.write(&delta).unwrap();
    }));
    let result = worker.join().unwrap();
    if let Err(panic) = written {
        std::panic::resume_unwind(panic);
    }
    let Err(error) = result else {
        panic!("changed request must not receive a durable ACK");
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .message
            .contains("request changed before acknowledgement")
    );
    assert!(
        !state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .registered
            .contains(run_id)
    );
    assert!(
        load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap()
            .test_run_registrations
            .contains_key(run_id),
        "the rejected ACK does not undo its earlier real commit"
    );
}
