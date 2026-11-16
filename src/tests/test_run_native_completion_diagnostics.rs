// Behavioral diagnostics beside test_run_native_completion.rs, split from
// test_run_registration.rs. Reuses real registration, artifact refresh and
// HTTP detail owners; disposable provider telemetry never grants credit.
use super::native_completion::native_run;
use super::observed_terminal::{LAUNCH, assert_no_credit, finish_artifact, launcher, start};
use super::*;

const PRIVATE_DIAGNOSTIC: &str = "producer-private-sentinel://not-for-publication";

async fn detail(fixture: &RunFixture, id: &str) -> Value {
    let (status, body): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .uri(format!("/api/test-runs/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn assert_safe_refusal(refusal: &Value, phase: &str, code: &str, root: &FsPath) {
    assert_eq!(refusal["phase"], phase, "{refusal}");
    assert_eq!(refusal["code"], code, "{refusal}");
    let observed = refusal["observed"]
        .as_object()
        .expect("safe observed facts");
    assert!(!observed.is_empty());
    assert!(
        observed
            .values()
            .all(|value| value.is_boolean() || value.is_u64())
    );
    assert!(
        refusal["remedy"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    let serialized = serde_json::to_string(refusal).unwrap();
    assert!(serialized.len() <= 2048, "bounded diagnostic");
    assert!(!serialized.contains(PRIVATE_DIAGNOSTIC));
    assert!(!serialized.contains(&test_run_display_path(root)));
    assert!(!serialized.contains("private-runtime-sentinel"));
}

async fn assert_native_refusal(fixture: &RunFixture, id: &str, code: &str) {
    let response = detail(fixture, id).await;
    let refusal = &response["detail"]["nativeCompletionRefusal"];
    assert_safe_refusal(refusal, "nativeCompletion", code, &fixture.root);
    let registration = fixture.state.inner.lock().unwrap().test_run_registrations[id].clone();
    assert!(registration.native_completion.is_none());
    let stored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let durable = serde_json::to_value(&stored.test_run_registrations[id]).unwrap();
    assert_eq!(&durable["nativeCompletionRefusal"], refusal);
    assert!(
        stored.test_run_registrations[id]
            .native_completion
            .is_none()
    );
    assert_no_credit(fixture);
}

fn provider_id(fixture: &RunFixture, id: &str) -> String {
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .values()
        .find(|row| {
            row.artifact
                .as_ref()
                .is_some_and(|artifact| artifact.run_id == id)
        })
        .unwrap()
        .execution_id
        .clone()
}

#[tokio::test]
async fn terminal_without_native_origin_reports_the_missing_prerequisite() {
    let (fixture, _, id, directory) = native_run("diagnostic-no-origin", true);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .update(&id, |row| {
            row.native_origin = None;
        });
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert_native_refusal(&fixture, &id, "native_origin_missing").await;
}

#[tokio::test]
async fn terminal_without_provider_reports_missing_observation_without_adopting_results() {
    let (fixture, _, id, directory) = native_run("diagnostic-no-provider", false);
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert_native_refusal(&fixture, &id, "provider_observation_missing").await;
}

#[tokio::test]
async fn terminal_without_runtime_reports_missing_identity_without_exposing_tokens() {
    let (fixture, _, id, directory) = native_run("diagnostic-no-runtime", true);
    let execution = provider_id(&fixture, &id);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .update(&execution, |row| {
            row.runtime_identity = None;
        });
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert_native_refusal(&fixture, &id, "provider_runtime_missing").await;
}

#[tokio::test]
async fn terminal_with_wrong_origin_session_reports_session_mismatch() {
    let (fixture, _, id, directory) = native_run("diagnostic-session-mismatch", true);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .update(&id, |row| {
            row.native_origin.as_mut().unwrap().native_session_id =
                "other-original-session".to_owned();
        });
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert_native_refusal(&fixture, &id, "registration_session_mismatch").await;
}

#[tokio::test]
async fn terminal_with_wrong_origin_project_reports_project_mismatch() {
    let (fixture, _, id, directory) = native_run("diagnostic-project-mismatch", true);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .update(&id, |row| {
            row.native_origin.as_mut().unwrap().project_id = "other-original-project".to_owned();
        });
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert_native_refusal(&fixture, &id, "registration_project_mismatch").await;
}

#[tokio::test]
async fn terminal_with_mismatched_provider_reports_association_refusal() {
    let (fixture, _, id, directory) = native_run("diagnostic-provider-mismatch", true);
    let execution = provider_id(&fixture, &id);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .update(&execution, |row| {
            row.summary.owner_session_id = Some("other-provider-owner".to_owned());
            row.runtime_identity = Some("private-runtime-sentinel".to_owned());
        });
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert_native_refusal(&fixture, &id, "provider_association_mismatch").await;
}

#[tokio::test]
async fn terminal_with_changed_request_reports_registration_mismatch() {
    let (fixture, _, id, directory) = native_run("diagnostic-request-mismatch", true);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .update(&id, |row| {
            row.request_identity = sha256_hex(b"different immutable request");
        });
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert_native_refusal(&fixture, &id, "registration_artifact_mismatch").await;
}

#[tokio::test]
async fn malformed_terminal_facts_report_validation_failure_without_copying_producer_text() {
    let (fixture, _, id, directory) = native_run("diagnostic-invalid-terminal", true);
    finish_artifact(&directory, &id, false);
    let path = directory.join("results.json");
    let mut results: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    results["ended"] = json!("not-a-timestamp");
    results["error"] = json!(PRIVATE_DIAGNOSTIC);
    fs::write(path, serde_json::to_vec(&results).unwrap()).unwrap();
    fixture.refresh(&[]);
    assert_native_refusal(&fixture, &id, "terminal_facts_invalid").await;
}

#[tokio::test]
async fn duplicate_original_provider_executions_report_ambiguity_without_choosing_one() {
    let (fixture, session, id, directory) = native_run("diagnostic-ambiguous", true);
    start(&fixture, &session, "second-diagnostic-tool", LAUNCH);
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        "second-diagnostic-tool",
        LAUNCH,
        &format!("RUN {}", test_run_display_path(&directory)),
        Some(EngramCommandExit::NotFinished),
        false,
    );
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert_native_refusal(&fixture, &id, "provider_execution_ambiguous").await;
}

async fn artifact_refusal(empty_worktree: bool) {
    let fixture = RunFixture::new("diagnostic-artifact-refusal");
    let session = running_owner(&fixture, "artifact-diagnostic-runtime");
    native_origin::bind_native_host(&fixture);
    let execution = start(&fixture, &session, "artifact-diagnostic-tool", LAUNCH);
    let (id, directory) = launcher(&fixture, &session);
    let locator = if empty_worktree {
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .update(&execution, |row| {
                row.summary.worktree.clear();
            });
        directory.clone()
    } else {
        // Real but foreign location: the explicit locator is never guessed
        // into the registered directory, even though the final component agrees.
        let foreign = fixture.root.join("foreign-private-location").join(&id);
        fs::create_dir_all(&foreign).unwrap();
        foreign
    };
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        "artifact-diagnostic-tool",
        LAUNCH,
        &format!("RUN {}", test_run_display_path(&locator)),
        Some(EngramCommandExit::NotFinished),
        false,
    );
    let response = detail(&fixture, &execution).await;
    let refusal = &response["detail"]["nativeCompletionRefusal"];
    assert_safe_refusal(
        refusal,
        "artifactAssociation",
        if empty_worktree {
            "artifact_worktree_unavailable"
        } else {
            "artifact_directory_mismatch"
        },
        &fixture.root,
    );
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let durable = serde_json::to_value(&loaded.test_run_observations[&execution]).unwrap();
    assert_eq!(&durable["nativeCompletionRefusal"], refusal);
    assert!(loaded.test_run_observations[&execution].artifact.is_none());
    assert!(
        loaded.test_run_registrations[&id]
            .native_completion
            .is_none()
    );
    assert_no_credit(&fixture);
}

#[tokio::test]
async fn artifact_without_start_worktree_reports_the_deciding_placement_prerequisite() {
    artifact_refusal(true).await;
}

#[tokio::test]
async fn foreign_explicit_artifact_reports_directory_mismatch_without_attaching() {
    artifact_refusal(false).await;
}

#[tokio::test]
async fn valid_native_retention_keeps_original_identity_durable_replay_and_no_refusal() {
    for success in [true, false] {
        let (fixture, session, id, directory) = native_run("diagnostic-valid-control", true);
        let execution = provider_id(&fixture, &id);
        finish_artifact(&directory, &id, success);
        fixture.refresh(&[]);
        let original = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
        let completion = original.native_completion.as_ref().unwrap();
        assert_eq!(completion.execution_id, execution);
        assert_eq!(completion.completion.origin.runner_session_id, session);
        assert_eq!(
            completion.completion.result.exit,
            Some(if success { 0 } else { 101 })
        );
        completion.completion.validate().unwrap();
        assert!(detail(&fixture, &id).await["detail"]["nativeCompletionRefusal"].is_null());
        fixture
            .state
            .register_test_run(&session, registration_request(&id, &directory))
            .unwrap();
        fixture.refresh(&[]);
        let loaded = load_state(fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.test_run_registrations[&id].native_completion,
            original.native_completion
        );
        assert_no_credit(&fixture);
    }
}

#[tokio::test]
async fn launch_only_provider_does_not_report_a_terminal_refusal_or_invent_completion() {
    let (fixture, _, id, _) = native_run("diagnostic-pending-control", true);
    fixture.refresh(&[717]);
    assert!(detail(&fixture, &id).await["detail"]["nativeCompletionRefusal"].is_null());
    assert!(
        fixture.state.inner.lock().unwrap().test_run_registrations[&id]
            .native_completion
            .is_none()
    );
    assert_no_credit(&fixture);
}
