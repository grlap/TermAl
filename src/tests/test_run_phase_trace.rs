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
    assert!(response["selectionLimit"].is_null());
    assert_eq!(response["selectionTruncated"], false);
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

#[tokio::test]
async fn recent_rescans_are_discoverable_without_a_registration_or_known_operation() {
    let state = test_app_state();
    assert!(!state.refresh_test_runs());
    let app = app_router(state.clone());
    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri("/api/test-run-diagnostics?recent=rescans&limit=1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the idle rescan must be discoverable without a registration: {response}"
    );
    assert_eq!(response["serverInstanceId"], state.server_instance_id);
    assert_eq!(response["selectionLimit"], 1);
    assert_eq!(response["selectionTruncated"], false);
    let traces = response["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 1);
    let trace = &traces[0];
    assert_eq!(trace["kind"], "rescan");
    assert!(trace["runId"].is_null());
    assert!(!trace["traceId"].as_str().unwrap().is_empty());
    assert!(!trace["operationId"].as_str().unwrap().is_empty());
    assert_eq!(trace["completed"], true);
    for phase in ["reads_done", "commit_acquired", "published", "finished"] {
        assert!(
            trace["phases"]
                .as_array()
                .unwrap()
                .iter()
                .any(|point| point["phase"] == phase),
            "missing {phase}: {trace}"
        );
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

async fn read_phase_diagnostics(state: &AppState, uri: &str) -> (StatusCode, Value) {
    request_json(
        &app_router(state.clone()),
        Request::builder().uri(uri).body(Body::empty()).unwrap(),
    )
    .await
}

#[tokio::test]
async fn recent_rescan_discovery_bounds_selection_and_preserves_exact_queries() {
    let state = test_app_state();
    for index in 0..TEST_RUN_TRACE_CAPACITY + 3 {
        let trace = state.test_run_phase_traces.begin(
            "rescan",
            None,
            &format!("scan-{index}"),
            std::time::Instant::now(),
        );
        if index == TEST_RUN_TRACE_CAPACITY + 2 {
            for _ in 0..TEST_RUN_TRACE_PHASES + 1 {
                trace.mark("phase");
            }
        }
        drop(trace);
        drop(state.test_run_phase_traces.begin(
            "registration",
            Some("test-unrelated"),
            "unrelated-registration",
            std::time::Instant::now(),
        ));
    }
    let revision = state.inner.lock().unwrap().revision;
    let (status, response) =
        read_phase_diagnostics(&state, "/api/test-run-diagnostics?recent=rescans&limit=2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["selectionLimit"], 2);
    assert_eq!(response["selectionTruncated"], true);
    let traces = response["traces"].as_array().unwrap();
    assert_eq!(traces.len(), 2);
    for (offset, trace) in traces.iter().enumerate() {
        assert_eq!(trace["kind"], "rescan");
        assert!(trace["runId"].is_null());
        assert_eq!(
            trace["operationId"],
            format!("scan-{}", TEST_RUN_TRACE_CAPACITY + 2 - offset)
        );
        assert!(trace["phases"].as_array().unwrap().len() <= TEST_RUN_TRACE_PHASES);
        let mut keys: Vec<_> = trace
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "completed",
                "kind",
                "operationId",
                "phases",
                "runId",
                "traceId",
                "truncated"
            ]
        );
    }
    assert_eq!(traces[0]["truncated"], true);
    assert_eq!(
        traces[0]["phases"].as_array().unwrap().last().unwrap()["phase"],
        "finished"
    );
    for (uri, expected_count, expected_limit) in [
        (
            "/api/test-run-diagnostics?recent=rescans",
            TEST_RUN_RECENT_RESCAN_LIMIT,
            TEST_RUN_RECENT_RESCAN_LIMIT,
        ),
        (
            "/api/test-run-diagnostics?recent=rescans&limit=256",
            TEST_RUN_TRACE_CAPACITY / 2,
            TEST_RUN_TRACE_CAPACITY,
        ),
    ] {
        let (status, response) = read_phase_diagnostics(&state, uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["traces"].as_array().unwrap().len(), expected_count);
        assert_eq!(response["selectionLimit"], expected_limit);
        assert_eq!(
            response["selectionTruncated"],
            expected_count == expected_limit
        );
    }
    let exact = format!(
        "/api/test-run-diagnostics?operationId=scan-{}",
        TEST_RUN_TRACE_CAPACITY + 2
    );
    let (status, response) = read_phase_diagnostics(&state, &exact).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        response["traces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|trace| trace["traceId"] == traces[0]["traceId"])
    );
    assert!(
        response["traces"]
            .as_array()
            .unwrap()
            .iter()
            .all(|trace| trace["kind"] == "rescan")
    );
    assert!(response["selectionLimit"].is_null());
    assert_eq!(response["selectionTruncated"], false);
    let (status, response) =
        read_phase_diagnostics(&state, "/api/test-run-diagnostics?operationId=scan-0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(response["traces"].as_array().unwrap().is_empty());
    assert_eq!(state.inner.lock().unwrap().revision, revision);
    assert_eq!(
        state.test_run_phase_traces.0.lock().unwrap().entries.len(),
        TEST_RUN_TRACE_CAPACITY
    );
}

#[tokio::test]
async fn recent_rescan_discovery_rejects_ambiguous_or_unbounded_queries() {
    let state = test_app_state();
    for uri in [
        "/api/test-run-diagnostics?recent=rescans&limit=0",
        "/api/test-run-diagnostics?recent=rescans&limit=257",
        "/api/test-run-diagnostics?recent=rescans&limit=-1",
        "/api/test-run-diagnostics?recent=rescans&limit=not-a-number",
        "/api/test-run-diagnostics?recent=registrations",
        "/api/test-run-diagnostics?limit=1",
        "/api/test-run-diagnostics?runId=test-one&limit=1",
        "/api/test-run-diagnostics?operationId=one&limit=1",
        "/api/test-run-diagnostics?recent=rescans&runId=test-one",
        "/api/test-run-diagnostics?recent=rescans&operationId=one",
        "/api/test-run-diagnostics?recent=rescans&unknown=value",
    ] {
        let (status, _) = read_phase_diagnostics(&state, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
    }
}

#[tokio::test]
async fn recent_rescan_discovery_qualifies_inflight_marks_and_restart_reset() {
    let state = test_app_state();
    let trace = state.test_run_phase_traces.begin(
        "rescan",
        None,
        "inflight-scan",
        std::time::Instant::now(),
    );
    trace.mark("commit_acquired");
    let (status, before) =
        read_phase_diagnostics(&state, "/api/test-run-diagnostics?recent=rescans").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(before["serverInstanceId"], state.server_instance_id);
    assert_eq!(before["traces"][0]["completed"], false);
    assert_eq!(
        before["traces"][0]["phases"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["phase"],
        "commit_acquired"
    );
    trace.mark("published");
    drop(trace);
    let (status, after) =
        read_phase_diagnostics(&state, "/api/test-run-diagnostics?recent=rescans").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        after["traces"][0]["traceId"],
        before["traces"][0]["traceId"]
    );
    assert_eq!(after["traces"][0]["completed"], true);
    let restarted = test_app_state();
    let (status, reset) =
        read_phase_diagnostics(&restarted, "/api/test-run-diagnostics?recent=rescans").await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(reset["serverInstanceId"], before["serverInstanceId"]);
    assert!(reset["traces"].as_array().unwrap().is_empty());
    assert!(
        reset["limitations"]
            .as_str()
            .unwrap()
            .contains("reset on restart")
    );
}

#[test]
fn registration_trace_survives_cancellation_of_the_actual_http_reply() {
    let mut fixture = FenceFixture::new();
    let mut state = fixture.app_state();
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
    let mut state = fixture.app_state();
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
