// Owns cached-request identity and query content-fence regression controls.
// Keeps the unchanged fail-first locator/containment tests separate.
// Split from the registration tests and uses their owned artifact fixture.
use super::*;

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
        .set_times(fs::FileTimes::new().set_modified(
            std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        ))
        .unwrap();
    fixture.refresh(&[std::process::id()]);
    let app = app_router(fixture.state.clone());
    let query = || Request::builder()
        .uri(format!("/api/test-runs/{run_id}"))
        .body(Body::empty())
        .unwrap();
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
            let mut inner = changed_state.inner.try_lock_now()
                .expect("native path checks must run outside StateInner");
            inner.test_run_registrations.get_mut(run_id).unwrap().monitoring_interrupted_at =
                Some("2026-10-14T01:01:00.000Z".to_owned());
        }));
    });
    let mut diagnostics = TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id);
    let error = test_run_exact_query(&fixture.state, run_id, &mut diagnostics)
        .err().expect("a superseded content fence must not select the old index entry");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(TEST_TEST_RUN_QUERY_CANONICALIZED.with(|slot| slot.borrow().is_none()));
}
