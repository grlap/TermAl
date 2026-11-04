//! Origin registration follows the native session's configured clone project.
//! Uses the real registration owner; does not authorize paths or remote credit.
use super::*;

fn independent_clone(fixture: &RunFixture) -> (PathBuf, String, String) {
    let clone = fixture.root.join("independent-clone");
    run_git_test_command(
        &fixture.root,
        &[
            "clone",
            "--quiet",
            "--no-hardlinks",
            fixture.root.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    let project = create_test_project(&fixture.state, &clone, "Independent clone");
    let session = create_test_project_session(&fixture.state, Agent::Codex, &project, &clone);
    (clone, project, session)
}

#[test]
fn independent_clone_registration_requires_its_native_clone_project_binding() {
    let fixture = RunFixture::new("native-clone-project");
    let unrelated_session = owner(&fixture);
    let (clone, project, session) = independent_clone(&fixture);
    let run_id = "test-independent-native-clone";
    let mut request = full_request(&clone, "2026-11-03T10:00:00.000Z");
    request["owner"] = json!(session);
    request["creatorPid"] = json!(std::process::id());
    request["detached"] = json!(true);
    let directory = write_run(
        &clone.join(".git").join("review-runs"),
        run_id,
        request,
        None,
    );

    let error = fixture
        .state
        .register_test_run(&unrelated_session, registration_request(run_id, &directory))
        .err()
        .expect("another project's session cannot authorize the clone path");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message.split(';').next(),
        Some("test run directory is outside this project's run directories")
    );
    assert!(error
        .message
        .split("; ")
        .any(|field| field == "phase=validate_target_directory"));
    assert!(!fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .contains_key(run_id));

    let ack = fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .expect("the native session bound to the clone project must register it");
    assert!(ack.registered);
    let inner = fixture.state.inner.lock().unwrap();
    let row = inner.test_run_registrations.get(run_id).unwrap();
    assert_eq!(row.project_id.as_deref(), Some(project.as_str()));
    assert_eq!(row.initiating_session_id, session);
    assert_eq!(row.owning_session_id, session);
}

#[test]
fn correct_clone_binding_cannot_substitute_for_immutable_request_ownership() {
    let fixture = RunFixture::new("native-clone-request-owner");
    let unrelated_session = owner(&fixture);
    let (clone, _, session) = independent_clone(&fixture);
    let run_id = "test-independent-wrong-owner";
    let mut request = full_request(&clone, "2026-11-03T10:00:00.000Z");
    request["owner"] = json!(unrelated_session);
    request["creatorPid"] = json!(std::process::id());
    request["detached"] = json!(true);
    let directory = write_run(
        &clone.join(".git").join("review-runs"),
        run_id,
        request,
        None,
    );
    let error = fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .err()
        .expect("a valid clone project does not replace the request's native owner");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(
        error.message.split(';').next(),
        Some("test run identity or initiating session does not match")
    );
    assert!(error
        .message
        .split("; ")
        .any(|field| field == "phase=parse_request"));
    assert!(!fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .contains_key(run_id));
}

#[test]
fn correct_clone_binding_refuses_a_request_root_in_another_repository() {
    let fixture = RunFixture::new("clone-wrong-root");
    let (clone, _, session) = independent_clone(&fixture);
    let run_id = "test-clone-wrong-root";
    let mut request = full_request(&fixture.root, "2026-11-03T10:00:00.000Z");
    request["owner"] = json!(session);
    request["creatorPid"] = json!(std::process::id());
    request["detached"] = json!(true);
    let directory = write_run(
        &clone.join(".git").join("review-runs"),
        run_id,
        request,
        None,
    );
    let error = fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .err()
        .expect("valid run placement cannot authorize another repository's request root");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message.split(';').next(),
        Some("test run worktree is outside the configured project")
    );
    assert!(error
        .message
        .split("; ")
        .any(|field| field == "phase=index_request"));
    assert!(fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .is_empty());
}

#[test]
fn correct_clone_binding_refuses_a_directory_named_for_another_run() {
    let fixture = RunFixture::new("clone-wrong-leaf");
    let (clone, _, session) = independent_clone(&fixture);
    let run_id = "test-clone-wrong-leaf";
    let mut request = full_request(&clone, "2026-11-03T10:00:00.000Z");
    request["owner"] = json!(session);
    request["creatorPid"] = json!(std::process::id());
    request["detached"] = json!(true);
    let directory = write_run(
        &clone.join(".git").join("review-runs"),
        run_id,
        request,
        None,
    );
    let renamed = directory.with_file_name("test-clone-other-leaf");
    fs::rename(&directory, &renamed).unwrap();
    let error = fixture
        .state
        .register_test_run(&session, registration_request(run_id, &renamed))
        .err()
        .expect("a matching request cannot authorize a directory named for another run");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message.split(';').next(),
        Some("test run directory is outside this project's run directories")
    );
    assert!(error
        .message
        .split("; ")
        .any(|field| field == "phase=validate_target_directory"));
    assert!(fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .is_empty());
}
