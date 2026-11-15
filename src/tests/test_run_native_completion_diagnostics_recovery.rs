// Recovery controls beside the native-completion diagnostics regressions.
// Existing registration and provider owners supply all facts; diagnostics
// neither fabricate authority nor latch a refusal after its facts recover.
use super::native_completion::native_run;
use super::observed_terminal::{LAUNCH, assert_no_credit, finish_artifact, launcher, start};
use super::*;

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

#[tokio::test]
async fn restored_original_runtime_clears_refusal_and_retains_only_original_completion() {
    let (fixture, _, id, directory) = native_run("diagnostic-runtime-recovery", true);
    let original = fixture
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
        .clone();
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .update(&original.execution_id, |row| {
            row.runtime_identity = None;
        });
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    assert_eq!(
        detail(&fixture, &id).await["detail"]["nativeCompletionRefusal"]["code"],
        "provider_runtime_missing"
    );
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .update(&original.execution_id, |row| {
            row.runtime_identity = original.runtime_identity.clone();
        });
    fixture.refresh(&[]);
    let retained = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    assert_eq!(
        retained.native_completion.as_ref().unwrap().execution_id,
        original.execution_id
    );
    assert!(detail(&fixture, &id).await["detail"]["nativeCompletionRefusal"].is_null());
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        loaded.test_run_registrations[&id].native_completion,
        retained.native_completion
    );
    assert!(serde_json::to_value(&loaded.test_run_registrations[&id]).unwrap()["nativeCompletionRefusal"].is_null());
    assert_no_credit(&fixture);
}

#[test]
fn diagnostic_append_during_registration_ack_keeps_the_original_durable_ack() {
    let (fixture, session, id, directory) = native_run("diagnostic-ack-race", true);
    let original = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    let execution = fixture
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
        .clone();
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
    let scanner = fixture.state.clone();
    TEST_TEST_RUN_REGISTRATION_RECHECKED.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            scanner.refresh_test_runs_with(&|_, _| false, &|_, _| {});
        }));
    });
    let ack = fixture
        .state
        .register_test_run(&session, registration_request(&id, &directory))
        .unwrap();
    assert!(ack.registered);
    assert!(TEST_TEST_RUN_REGISTRATION_RECHECKED.with(|slot| slot.borrow().is_none()));
    let after = fixture.state.inner.lock().unwrap().test_run_registrations[&id].clone();
    assert!(after.native_completion.is_none());
    assert_eq!(
        serde_json::to_value(&after).unwrap()["nativeCompletionRefusal"]["code"],
        "provider_runtime_missing"
    );
    assert!(after.matches_registration_snapshot(&original));
    let mut changed = after.clone();
    changed.request_identity = sha256_hex(b"another request");
    assert!(!changed.matches_registration_snapshot(&original));
    assert_no_credit(&fixture);
}

#[tokio::test]
async fn original_locator_after_foreign_locator_clears_only_the_artifact_refusal() {
    let fixture = RunFixture::new("diagnostic-locator-recovery");
    let session = running_owner(&fixture, "diagnostic-original-runtime");
    native_origin::bind_native_host(&fixture);
    let execution = start(&fixture, &session, "original-diagnostic-tool", LAUNCH);
    let (id, directory) = launcher(&fixture, &session);
    let foreign = fixture.root.join("foreign").join(&id);
    fs::create_dir_all(&foreign).unwrap();
    for (locator, expected) in [
        (&foreign, Some("artifact_directory_mismatch")),
        (&directory, None),
    ] {
        fixture.state.note_engram_command_finished_with_capture(
            &session,
            &EngramObservationProvenance::Ambient,
            "original-diagnostic-tool",
            LAUNCH,
            &format!("RUN {}", test_run_display_path(locator)),
            Some(EngramCommandExit::NotFinished),
            false,
        );
        let response = detail(&fixture, &execution).await;
        assert_eq!(
            response["detail"]["nativeCompletionRefusal"]["code"].as_str(),
            expected
        );
    }
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let retained = loaded.test_run_registrations[&id]
        .native_completion
        .as_ref()
        .unwrap();
    assert_eq!(retained.execution_id, execution);
    assert_eq!(retained.completion.origin.runner_session_id, session);
    assert_eq!(
        loaded.test_run_observations[&execution]
            .artifact
            .as_ref()
            .unwrap()
            .run_id,
        id
    );
    assert!(detail(&fixture, &id).await["detail"]["nativeCompletionRefusal"].is_null());
    assert_no_credit(&fixture);
}
