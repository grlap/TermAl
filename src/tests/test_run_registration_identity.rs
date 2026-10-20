// Owns native registration containment and exact-query identity regressions.
// Does not change scanner duplicate selection or simulate durable acceptance.
// Split from the registration tests and uses their real artifact fixtures.
use super::*;

#[cfg(unix)]
#[test]
fn registration_target_refuses_a_literal_backslash_sibling() {
    let root = TestTempRoot::create("registration-native-backslash");
    let common = root.join("git");
    fs::create_dir_all(&common).unwrap();
    let outside = root.join("git\\review-runs").join("test-outside");
    fs::create_dir_all(&outside).unwrap();
    assert!(!test_run_registration_target(
        &common,
        &fs::canonicalize(outside).unwrap()
    ));
}

#[cfg(unix)]
#[test]
fn registration_target_refuses_distinct_non_utf8_parent_components() {
    use std::os::unix::ffi::OsStringExt;

    // No mkdir: APFS only creates valid UTF8 names. The native path guard
    // must still distinguish byte components without a lossy text round-trip.
    let common = PathBuf::from(std::ffi::OsString::from_vec(b"/git\x80".to_vec()));
    let other = PathBuf::from(std::ffi::OsString::from_vec(b"/git\x81".to_vec()));
    assert_eq!(common.to_string_lossy(), other.to_string_lossy());
    assert!(test_run_registration_relative_parent(&common, &other.join("review-runs")).is_none());
    assert!(test_run_registration_relative_parent(&common, &common.join("review-runs")).is_some());
}

#[cfg(target_os = "linux")]
#[test]
fn registration_target_refuses_non_utf8_siblings_on_a_byte_name_filesystem() {
    use std::os::unix::ffi::OsStringExt;

    let root = TestTempRoot::create("registration-native-byte-files");
    let common = root.join(std::ffi::OsString::from_vec(b"git\x80".to_vec()));
    fs::create_dir_all(&common).unwrap();
    let outside = root
        .join(std::ffi::OsString::from_vec(b"git\x81".to_vec()))
        .join("review-runs")
        .join("test-outside");
    fs::create_dir_all(&outside).unwrap();
    assert!(!test_run_registration_target(
        &common,
        &fs::canonicalize(outside).unwrap()
    ));
}

async fn assert_registered_query(
    fixture: &RunFixture,
    run_id: &str,
    directory: &FsPath,
    session: &str,
    source: &str,
) {
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .uri(format!("/api/test-runs/{run_id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["source"], source, "{response}");
    assert_eq!(response["run"]["runId"], run_id);
    assert_eq!(response["run"]["ownerSessionId"], session);
    assert_eq!(response["run"]["startedAt"], "2026-10-14T01:00:00.000Z");
    assert_eq!(response["run"]["registrationState"], "registered");
    assert_eq!(response["run"]["trackingDurability"], "durable");
    assert_eq!(
        fs::canonicalize(response["run"]["runDir"].as_str().unwrap()).unwrap(),
        fs::canonicalize(directory).unwrap()
    );
}

#[tokio::test]
async fn indexed_matching_registration_keeps_the_index_detail() {
    let fixture = RunFixture::new("registration-index-matching");
    let session = owner(&fixture);
    let run_id = "test-index-matching-registration";
    let directory = registered_run(&fixture, &session, run_id);
    fixture.refresh(&[std::process::id()]);
    fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .unwrap();
    assert_registered_query(&fixture, run_id, &directory, &session, "index").await;
}

#[tokio::test]
async fn indexed_copy_cannot_shadow_a_different_durable_registration_locator() {
    let fixture = RunFixture::new("registration-index-copy");
    let session = owner(&fixture);
    let run_id = "test-index-copied-registration";
    let main_copy = registered_run(&fixture, &session, run_id);
    let linked_admin = fixture.root.join(".git").join("worktrees").join("copy");
    fs::create_dir_all(&linked_admin).unwrap();
    fs::write(linked_admin.join("commondir"), "../..\n").unwrap();
    // Copy the immutable request exactly; only the artifact locator differs.
    let request =
        serde_json::from_slice(&fs::read(main_copy.join("request.json")).unwrap()).unwrap();
    let registered = write_run(&linked_admin.join("review-runs"), run_id, request, None);
    fixture.refresh(&[std::process::id()]);
    let indexed_directory = || {
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .find(run_id)
            .unwrap()
            .disk
            .run_dir
            .clone()
    };
    assert_eq!(
        fs::canonicalize(indexed_directory()).unwrap(),
        fs::canonicalize(&main_copy).unwrap()
    );
    let (status, ack): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session}/test-runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "runId": run_id, "runDirectory": test_run_display_path(&registered)
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{ack}");
    assert_eq!(ack["registered"], true);
    let restored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        fs::canonicalize(&restored.test_run_registrations[run_id].run_directory).unwrap(),
        fs::canonicalize(&registered).unwrap()
    );
    assert_registered_query(&fixture, run_id, &registered, &session, "registration").await;
    fixture.refresh(&[std::process::id()]);
    assert_eq!(
        fs::canonicalize(indexed_directory()).unwrap(),
        fs::canonicalize(&main_copy).unwrap(),
        "the fix must preserve the scanner's deterministic first-copy policy"
    );
    assert_registered_query(&fixture, run_id, &registered, &session, "registration").await;
}
