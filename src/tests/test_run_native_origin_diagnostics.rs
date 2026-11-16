// Missing-origin diagnostics report current capture prerequisites without
// adopting an origin for a historical or ineligible original registration.
use super::observed_terminal::{LAUNCH, assert_no_credit, finish_artifact, launcher, start};
use super::*;

async fn refusal(fixture: &RunFixture, id: &str) -> Value {
    let (status, response): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .uri(format!("/api/test-runs/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    response["detail"]["nativeCompletionRefusal"].clone()
}

fn finished_without_origin(label: &str, host: bool, creation: bool) -> (RunFixture, String) {
    let fixture = RunFixture::new(label);
    let session = running_owner(&fixture, "native-origin-diagnostic-runtime");
    if host {
        native_origin::bind_native_host(&fixture);
    }
    if !creation {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].federation_native_creation_id.clear();
    }
    start(&fixture, &session, "origin-diagnostic-tool", LAUNCH);
    let (id, directory) = launcher(&fixture, &session);
    assert!(
        fixture.state.inner.lock().unwrap().test_run_registrations[&id]
            .native_origin
            .is_none(),
        "the real registration boundary must lack original capture"
    );
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        "origin-diagnostic-tool",
        LAUNCH,
        &format!("RUN {}", test_run_display_path(&directory)),
        Some(EngramCommandExit::NotFinished),
        false,
    );
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    (fixture, id)
}

fn assert_original_unqualified(fixture: &RunFixture, id: &str) {
    let inner = fixture.state.inner.lock().unwrap();
    let original = &inner.test_run_registrations[id];
    assert!(original.native_origin.is_none());
    assert!(original.native_completion.is_none());
    assert!(original.verification_record.is_none());
    drop(inner);
    assert_no_credit(fixture);
}

fn missing_historical_capture() -> (RunFixture, String) {
    let (fixture, _, id, directory) =
        super::native_completion::native_run("origin-diagnostic-old-capture", true);
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .update(&id, |row| row.native_origin = None);
    finish_artifact(&directory, &id, true);
    fixture.refresh(&[]);
    (fixture, id)
}

#[tokio::test]
async fn nonfederated_registration_reports_unavailable_host_capture_prerequisite() {
    let (fixture, id) = finished_without_origin("origin-diagnostic-no-host", false, true);
    let diagnostic = refusal(&fixture, &id).await;
    assert_eq!(diagnostic["code"], "native_host_identity_unavailable");
    assert_eq!(diagnostic["outcome"], "refused");
    assert_eq!(
        diagnostic["observed"],
        json!({
            "nativeOriginPresent":false, "currentHostIdentityValid":false,
            "currentSessionCreationValid":true,
        })
    );
    let (capable, legacy_id) = missing_historical_capture();
    assert_ne!(
        diagnostic["remedy"],
        refusal(&capable, &legacy_id).await["remedy"],
        "missing host capability needs guidance beyond retrying capture on this host"
    );
    let stored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&stored.test_run_registrations[&id]).unwrap()["nativeCompletionRefusal"],
        diagnostic
    );
    assert_original_unqualified(&fixture, &id);
}

#[tokio::test]
async fn nonnative_session_reports_unavailable_creation_capture_prerequisite() {
    let (fixture, id) = finished_without_origin("origin-diagnostic-no-creation", true, false);
    let diagnostic = refusal(&fixture, &id).await;
    assert_eq!(diagnostic["code"], "native_session_creation_unavailable");
    assert_eq!(diagnostic["outcome"], "refused");
    assert_eq!(
        diagnostic["observed"],
        json!({
            "nativeOriginPresent":false, "currentHostIdentityValid":true,
            "currentSessionCreationValid":false,
        })
    );
    let (capable, legacy_id) = missing_historical_capture();
    assert_ne!(
        diagnostic["remedy"],
        refusal(&capable, &legacy_id).await["remedy"],
        "missing session capability needs guidance beyond retrying capture in this session"
    );
    assert_original_unqualified(&fixture, &id);
}

#[tokio::test]
async fn missing_historical_capture_reports_current_capability_without_adopting_origin() {
    let (fixture, id) = missing_historical_capture();
    let diagnostic = refusal(&fixture, &id).await;
    assert_eq!(diagnostic["code"], "native_origin_missing");
    assert_eq!(
        diagnostic["observed"],
        json!({
            "nativeOriginPresent":false, "currentHostIdentityValid":true,
            "currentSessionCreationValid":true,
        })
    );
    assert_original_unqualified(&fixture, &id);
}
