// Exercises provider-owned refusal detail through the real history owner and
// SQLite reload. Final-unbound proof is custody, never native test credit.
use super::compaction::fill_unbound_history;
use super::observed_terminal::{assert_no_credit, start};
use super::*;

const FOCUSED: &str = "node scripts/test-launcher.mjs focused --input commit -- cargo test";

fn provider_refusal(label: &str) -> (RunFixture, TestRunCommandObservation) {
    let fixture = RunFixture::new(label);
    let session = running_owner(&fixture, "retained-diagnostic-runtime");
    let key = "retained-diagnostic-tool";
    let execution = start(&fixture, &session, key, FOCUSED);
    let foreign = fixture
        .root
        .join("foreign-locator")
        .join(format!("test-{}", Uuid::new_v4()));
    fs::create_dir_all(&foreign).unwrap();
    fixture.state.note_engram_command_finished_with_capture(
        &session,
        &EngramObservationProvenance::Ambient,
        key,
        FOCUSED,
        &format!("RUN {}", test_run_display_path(&foreign)),
        Some(EngramCommandExit::Code(0)),
        false,
    );
    let original = fixture.state.inner.lock().unwrap().test_run_observations[&execution].clone();
    assert_eq!(original.summary.state, TestRunState::Passed);
    assert!(original.artifact.is_none());
    assert!(test_run_final_unbound_valid(&original));
    assert!(original.evidence.is_empty());
    assert!(original.work_binding.is_none());
    assert_eq!(
        original.native_completion_refusal.as_ref().unwrap().code,
        TestRunNativeCompletionRefusalCode::ArtifactDirectoryMismatch
    );
    assert_no_credit(&fixture);
    (fixture, original)
}

async fn refusal_detail(fixture: &RunFixture, id: &str) -> Value {
    let (status, body): (StatusCode, Value) = request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .uri(format!("/api/test-runs/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["run"]["runId"], id);
    assert_eq!(body["run"]["state"], "passed");
    assert!(
        body["run"]["verificationRecordIds"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    body
}

fn reload(fixture: &RunFixture) {
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    *fixture.state.inner.lock().unwrap() = loaded;
    fixture.refresh(&[]);
}

#[tokio::test]
async fn live_provider_refusal_detail_survives_reload() {
    let (fixture, original) = provider_refusal("diagnostic-live-reload");
    let id = &original.execution_id;
    let before = refusal_detail(&fixture, id).await;
    let expected =
        serde_json::to_value(original.native_completion_refusal.as_ref().unwrap()).unwrap();
    assert_eq!(before["detail"]["nativeCompletionRefusal"], expected);
    reload(&fixture);
    let after = refusal_detail(&fixture, id).await;
    assert_eq!(after["detail"]["nativeCompletionRefusal"], expected);
    let inner = fixture.state.inner.lock().unwrap();
    let durable = &inner.test_run_observations[id];
    assert_eq!(durable.execution_id, original.execution_id);
    assert_eq!(durable.final_unbound_proof, original.final_unbound_proof);
    assert!(durable.artifact.is_none() && durable.evidence.is_empty());
    assert!(inner.test_run_registrations.is_empty());
    drop(inner);
    assert_no_credit(&fixture);
}

async fn retained_provider_refusal(reload_after_compaction: bool) {
    let (fixture, original) = provider_refusal("diagnostic-compaction-reload");
    let id = &original.execution_id;
    let expected =
        serde_json::to_value(original.native_completion_refusal.as_ref().unwrap()).unwrap();
    assert_eq!(
        refusal_detail(&fixture, id).await["detail"]["nativeCompletionRefusal"],
        expected
    );
    // Genuine later terminal observations cross the normal per-project cap;
    // no retained row, receipt or durability flag is inserted by this test.
    fill_unbound_history(&fixture, TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 1);
    fixture.refresh(&[]);
    {
        let inner = fixture.state.inner.lock().unwrap();
        let retained = &inner.test_run_compacted[id];
        assert!(retained.compacted && test_run_compact_is_installed(&*inner, retained));
        assert!(!inner.test_run_observations.contains_key(id));
        assert!(retained.registration.is_none());
        assert_eq!(retained.observations.len(), 1);
        assert_eq!(
            retained.observations[0],
            test_run_minimal_observation(original.clone())
        );
    }
    let loaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let durable = &loaded.test_run_compacted[id];
    assert!(!loaded.test_run_observations.contains_key(id));
    assert!(durable.registration.is_none());
    let owner = &durable.observations[0];
    assert_eq!(owner.execution_id, original.execution_id);
    assert_eq!(owner.runtime_identity, original.runtime_identity);
    assert_eq!(owner.final_unbound_proof, original.final_unbound_proof);
    assert_eq!(
        owner.native_completion_refusal,
        original.native_completion_refusal
    );
    assert!(test_run_final_unbound_valid(owner));
    assert!(owner.artifact.is_none() && owner.evidence.is_empty() && owner.work_binding.is_none());
    if reload_after_compaction {
        reload(&fixture);
    }
    assert_no_credit(&fixture);
    let response = refusal_detail(&fixture, id).await;
    assert_eq!(response["source"], "retained");
    assert_eq!(response["run"]["historyCompacted"], true);
    assert_eq!(
        response["detail"]["nativeCompletionRefusal"], expected,
        "the exact retained provider owner keeps its durable refusal visible"
    );
    assert_no_credit(&fixture);
}

#[tokio::test]
async fn compacted_provider_refusal_remains_visible_in_detail() {
    retained_provider_refusal(false).await;
}

#[tokio::test]
async fn compacted_provider_refusal_remains_visible_after_reload() {
    retained_provider_refusal(true).await;
}
