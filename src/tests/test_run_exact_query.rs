// Owns cached-request identity and query content-fence regression controls.
// Keeps the unchanged fail-first locator/containment tests separate.
// Split from the registration tests and uses their owned artifact fixture.
use super::*;

#[tokio::test]
async fn registration_view_keeps_exact_identity_before_and_after_indexing() {
    let fixture = RunFixture::new("registration-view-exact");
    let session = owner(&fixture);
    let id = "test-registration-view-exact";
    let directory = registered_run(&fixture, &session, id);
    fixture
        .state
        .register_test_run(&session, registration_request(id, &directory))
        .unwrap();
    let app = app_router(fixture.state.clone());
    for indexed in [false, true] {
        if indexed {
            fixture.refresh(&[std::process::id()]);
        }
        let (status, response): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri(format!("/api/test-runs/{id}?view=registration"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(
            response["source"],
            if indexed { "index" } else { "registration" }
        );
        assert_eq!(response["run"]["runId"], id);
        assert_eq!(response["run"]["ownerSessionId"], session);
        assert_eq!(response["run"]["initiatingSessionId"], session);
        assert_eq!(response["run"]["runDir"], test_run_display_path(&directory));
        assert_eq!(response["run"]["registrationState"], "registered");
        assert_eq!(response["run"]["trackingDurability"], "durable");
        assert!(response.get("detail").is_none());
    }
    for (target, expected) in [
        (
            format!("/api/test-runs/{id}?view=unsupported"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/test-runs/test-unknown?view=registration".to_owned(),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, _): (StatusCode, Value) = request_json(
            &app,
            Request::builder().uri(target).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, expected);
    }
}

// The launcher reads `bound` to say why an unbound run earns no credit; the
// view says whether the registration bound the run, never which work.
#[tokio::test]
async fn registration_view_says_whether_the_run_is_bound_without_naming_work() {
    let fixture = RunFixture::new("registration-view-bound");
    let app = app_router(fixture.state.clone());
    for (id, binding) in [
        ("test-registration-view-unbound", None),
        (
            "test-registration-view-bound",
            Some(EngramControlWorkBinding {
                root_execution_id: "bound-root".to_owned(),
                work_id: "bound-work".to_owned(),
                run_id: "bound-run".to_owned(),
                work_revision: 1,
                claim_id: "bound-claim".to_owned(),
                claim_fence: 1,
            }),
        ),
    ] {
        let session = owner(&fixture);
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.work_binding = binding.clone();
        }
        let directory = registered_run(&fixture, &session, id);
        fixture
            .state
            .register_test_run(&session, registration_request(id, &directory))
            .unwrap();
        let (status, response): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri(format!("/api/test-runs/{id}?view=registration"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(
            response["run"]["bound"],
            binding.is_some(),
            "{id}: {response}"
        );
        assert!(
            !response.to_string().contains("bound-work"),
            "{id}: the view names no work: {response}"
        );
        // Once compaction retains the registration, the view answers from the
        // retained row, which no longer appears among live registrations.
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let registration = inner.test_run_registrations.remove(id).unwrap();
            let summary = test_run_registered_fallback(&inner, &registration);
            inner.test_runs.compactions_durable.insert(id.to_owned());
            inner.test_run_compacted.insert(
                id.into(),
                TestRunCompacted {
                    compacted: true,
                    request_identity: Some(registration.request_identity.clone()),
                    summary,
                    registration: Some(registration),
                    observations: Vec::new(),
                    request_stamp: None,
                    results_stamp: None,
                    reason: "fixture retained registration".into(),
                    artifact_conflict: false,
                    artifact_unavailable: false,
                },
            );
        }
        let (status, retained): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri(format!("/api/test-runs/{id}?view=registration"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{retained}");
        assert_eq!(retained["source"], "retained", "{id}: {retained}");
        assert_eq!(
            retained["run"]["bound"],
            binding.is_some(),
            "{id}: {retained}"
        );
    }
}

// The view's last answer serves a retained run its first lookup missed: here
// compaction retains the run, with its evidence under revalidation, after that
// lookup and before the exact query rechecks. `bound` still comes from the
// retained row's durable registration, never a fixed false.
#[test]
fn registration_view_takes_bound_from_a_run_retained_after_its_first_lookup() {
    let fixture = RunFixture::new("registration-view-retained-late");
    for (id, binding) in [
        ("test-registration-retained-late-unbound", None),
        (
            "test-registration-retained-late-bound",
            Some(EngramControlWorkBinding {
                root_execution_id: "bound-root".to_owned(),
                work_id: "bound-work".to_owned(),
                run_id: "bound-run".to_owned(),
                work_revision: 1,
                claim_id: "bound-claim".to_owned(),
                claim_fence: 1,
            }),
        ),
    ] {
        let session = owner(&fixture);
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.work_binding = binding.clone();
        }
        let directory = registered_run(&fixture, &session, id);
        fixture
            .state
            .register_test_run(&session, registration_request(id, &directory))
            .unwrap();
        // Compaction is under way: the run has left the live registrations and
        // the durable set, and its retained row is not installed yet.
        let registration = {
            let mut inner = fixture.state.inner.lock().unwrap();
            inner.test_runs.registered.remove(id);
            inner.test_run_registrations.remove(id).unwrap()
        };
        let state = fixture.state.clone();
        let retained = id.to_owned();
        TEST_TEST_RUN_QUERY_CANONICALIZED.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let mut inner = state.inner.lock().unwrap();
                let summary = test_run_registered_fallback(&inner, &registration);
                inner.test_runs.compactions_durable.insert(retained.clone());
                inner
                    .test_runs
                    .compactions_revalidating
                    .insert(retained.clone());
                inner.test_run_compacted.insert(
                    retained.as_str().into(),
                    TestRunCompacted {
                        compacted: true,
                        request_identity: Some(registration.request_identity.clone()),
                        summary,
                        registration: Some(registration),
                        observations: Vec::new(),
                        request_stamp: None,
                        results_stamp: None,
                        reason: "fixture retained after the first lookup".into(),
                        artifact_conflict: false,
                        artifact_unavailable: false,
                    },
                );
            }));
        });
        let mut diagnostics =
            TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id);
        let response = get_test_run_observed_without_native_diagnostics(
            fixture.state.clone(),
            id,
            &mut diagnostics,
            false,
        )
        .unwrap()
        .0;
        assert!(
            TEST_TEST_RUN_QUERY_CANONICALIZED.with(|slot| slot.borrow().is_none()),
            "{id}: the run was retained between the lookups"
        );
        assert_eq!(response.source, "retained", "{id}");
        assert_eq!(response.registration_bound, binding.is_some(), "{id}");
    }
}

// `bound` is read inside the exact query's fence, from the registration that
// query serves: a registration still being published binds nothing, so an
// answer never pairs a pending registration with a binding.
#[test]
fn bound_comes_from_the_exact_query_snapshot_and_needs_a_durable_registration() {
    let fixture = RunFixture::new("registration-bound-snapshot");
    let session = owner(&fixture);
    let id = "test-registration-bound-snapshot";
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.work_binding = Some(EngramControlWorkBinding {
            root_execution_id: "bound-root".to_owned(),
            work_id: "bound-work".to_owned(),
            run_id: "bound-run".to_owned(),
            work_revision: 1,
            claim_id: "bound-claim".to_owned(),
            claim_fence: 1,
        });
    }
    let directory = registered_run(&fixture, &session, id);
    fixture
        .state
        .register_test_run(&session, registration_request(id, &directory))
        .unwrap();
    let exact = |fixture: &RunFixture| {
        let mut diagnostics =
            TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id);
        test_run_exact_query(&fixture.state, id, &mut diagnostics)
            .unwrap()
            .expect("the registered run is known")
    };
    let durable = exact(&fixture);
    assert!(durable.registration_bound, "a durable bound registration");
    // The same registration while it is still being published: the snapshot
    // is pending, and it binds nothing yet.
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_runs
        .registered
        .remove(id);
    let pending = exact(&fixture);
    assert_ne!(
        serde_json::to_value(&pending.run).unwrap()["trackingDurability"],
        "durable",
        "the snapshot is pending"
    );
    assert!(
        !pending.registration_bound,
        "a registration not yet durable binds nothing"
    );
}

// The run view's bound work list also gathers observations of the launch;
// `bound` follows the registration alone, so an observation that carries a
// binding cannot make an unbound registration read as bound.
#[tokio::test]
async fn registration_view_bound_ignores_a_binding_only_an_observation_carries() {
    let (fixture, id, _store, binding, native) =
        native_evidence_fixture("registration-view-observed-binding");
    record_native_evidence(&fixture, &native);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let mut row = inner.test_run_registrations.get_mut(&id).unwrap();
        row.work_binding = None;
        drop(row);
    }
    assert_eq!(
        fixture.summary(&id).registration.bound_work_ids,
        [binding.work_id.clone()],
        "precondition: the observation still carries the binding"
    );
    let app = app_router(fixture.state.clone());
    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .uri(format!("/api/test-runs/{id}?view=registration"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["run"]["bound"], false, "{response}");
}

#[tokio::test]
async fn an_older_index_request_at_the_same_locator_cannot_replace_the_registered_identity() {
    let fixture = RunFixture::new("registration-stale-request");
    let session = owner(&fixture);
    let run_id = "test-index-stale-request";
    let directory = registered_run(&fixture, &session, run_id);
    // Register before scanning an older cached copy: discovery intentionally
    // retains its immutable identity and would refuse a conflicting new launch.
    fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .unwrap();
    let path = directory.join("request.json");
    let current_request = fs::read(&path).unwrap();
    let mut earlier: Value = serde_json::from_slice(&current_request).unwrap();
    earlier["started"] = json!("2026-10-13T01:00:00.000Z");
    fs::write(&path, earlier.to_string()).unwrap();
    // A distinct old stamp makes catch-up independent of filesystem clock resolution.
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        )
        .unwrap();
    fixture.refresh(&[std::process::id()]);
    let app = app_router(fixture.state.clone());
    let query = || {
        Request::builder()
            .uri(format!("/api/test-runs/{run_id}"))
            .body(Body::empty())
            .unwrap()
    };
    let (status, response): (StatusCode, Value) = request_json(&app, query()).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["source"], "registration");
    assert_eq!(response["run"]["startedAt"], "2026-10-14T01:00:00.000Z");
    assert_eq!(response["run"]["registrationState"], "registered");
    // Ordinary rescan catches up; the exact query may then serve index detail.
    fs::write(&path, current_request).unwrap();
    fixture.refresh(&[std::process::id()]);
    let (status, response): (StatusCode, Value) = request_json(&app, query()).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["source"], "index");
    assert_eq!(response["run"]["startedAt"], "2026-10-14T01:00:00.000Z");
    assert_eq!(response["run"]["registrationState"], "registered");
}

#[test]
fn exact_query_rechecks_the_captured_registration_after_the_off_lock_path_check() {
    let fixture = RunFixture::new("registration-query-fence");
    let session = owner(&fixture);
    let run_id = "test-index-query-fence";
    let directory = registered_run(&fixture, &session, run_id);
    fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .unwrap();
    fixture.refresh(&[std::process::id()]);
    let changed_state = fixture.state.clone();
    TEST_TEST_RUN_QUERY_CANONICALIZED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            let mut inner = changed_state
                .inner
                .try_lock_now()
                .expect("native path checks must run outside StateInner");
            inner
                .test_run_registrations
                .get_mut(run_id)
                .unwrap()
                .monitoring_interrupted_at = Some("2026-10-14T01:01:00.000Z".to_owned());
        }));
    });
    let mut diagnostics = TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id);
    let error = test_run_exact_query(&fixture.state, run_id, &mut diagnostics)
        .err()
        .expect("a superseded content fence must not select the old index entry");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(TEST_TEST_RUN_QUERY_CANONICALIZED.with(|slot| slot.borrow().is_none()));
}
