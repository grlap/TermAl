// Operator monitoring dispositions through the HTTP boundary, on disposable
// observations and launcher artifacts. Does not stop real work or award credit.
use super::*;

fn start_observation(fixture: &RunFixture, session: &str, key: &str) -> String {
    start_command(fixture, session, key, "cargo test")
}

fn start_command(fixture: &RunFixture, session: &str, key: &str, command: &str) -> String {
    fixture.state.note_engram_command_started(
        session,
        &EngramObservationProvenance::Ambient,
        key,
        Some(command),
        Some(&test_run_display_path(&fixture.root)),
    );
    fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .values()
        .find(|row| row.tool_identity == sha256_hex(key.as_bytes()))
        .unwrap()
        .execution_id
        .clone()
}

fn request_for(fixture: &RunFixture, id: &str) -> Value {
    let row = fixture.state.inner.lock().unwrap().test_run_observations[id].clone();
    json!({"ownerSessionId": row.summary.owner_session_id,
        "startedAt": row.summary.started_at, "reason": "Original terminal event could not be recovered"})
}

async fn interrupt(fixture: &RunFixture, id: &str, body: Value) -> (StatusCode, Value) {
    request_json(
        &app_router(fixture.state.clone()),
        Request::builder()
            .method("POST")
            .uri(format!("/api/test-runs/{id}/interrupt-monitoring"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn monitoring_interruption_is_durable_idempotent_and_preserves_evidence_and_live_work() {
    let fixture = RunFixture::new("monitoring-disposition");
    let session = running_owner(&fixture, "monitoring-original-runtime");
    let id = start_observation(&fixture, &session, "selected");
    let other = start_observation(&fixture, &session, "still-live");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner
            .test_run_observations
            .update(&id, |row| {
                row.evidence.push(TestRunEvidenceAssociation {
                    store: EngramAuthorityStoreKey {
                        database_path: fixture.root.join("tracker.db"),
                        project_id: "fixture-project".to_owned(),
                    },
                    binding: EngramControlWorkBinding {
                        root_execution_id: "fixture-root".to_owned(),
                        work_id: "fixture-work".to_owned(),
                        run_id: "fixture-run".to_owned(),
                        work_revision: 1,
                        claim_id: "fixture-claim".to_owned(),
                        claim_fence: 1,
                    },
                    source_basis: EngramExecutionSourceBasis {
                        source_id: None,
                        workspace_id: fixture.root.to_string_lossy().into_owned(),
                        source_revision: "a".repeat(64),
                        source_root_generation: None,
                        source_root_state: None,
                    },
                    producer_observation_id: "b".repeat(64),
                    action_fingerprint: "c".repeat(64),
                    reported_outcome: EngramExecutionOutcome::Succeeded,
                    verification_record_id: Some("d".repeat(64)),
                    checkpoint_id: None,
                    confirmed_cursor: None,
                    lifecycle_read: None,
                })
            })
            .unwrap();
    }
    let before = fixture.state.inner.lock().unwrap().test_run_observations[&id].clone();
    let token = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .runtime
            .runtime_token()
    };
    let request = request_for(&fixture, &id);
    let mut events = fixture.state.subscribe_delta_events();
    let (status, response) = interrupt(&fixture, &id, request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["state"], "unknown");
    assert_eq!(response["interrupted"], true);
    assert!(response["endedAt"].is_null());
    assert!(response["exitCode"].is_null());
    let after = fixture.state.inner.lock().unwrap().test_run_observations[&id].clone();
    assert!(after.monitoring_interrupted_at.is_some());
    assert_eq!(after.evidence, before.evidence);
    assert_eq!(after.runtime_identity, before.runtime_identity);
    assert_eq!(after.tool_identity, before.tool_identity);
    assert_eq!(after.final_unbound_proof, before.final_unbound_proof);
    assert_eq!(fixture.summary(&other).state, TestRunState::Running);
    assert!(
        drain_test_run_deltas(&mut events)
            .iter()
            .any(|event| matches!(event,
        DeltaEvent::TestRunChanged { run, .. } if run.run_id == id && run.interrupted))
    );
    let (status, again) = interrupt(&fixture, &id, request.clone()).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_observations[&id],
        after
    );
    let mut changed = request;
    changed["reason"] = json!("A different retrospective explanation");
    assert_eq!(
        interrupt(&fixture, &id, changed).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_observations[&id],
        after
    );
    fixture.state.step_test_command_observation_persistence();
    {
        let inner = fixture.state.inner.lock().unwrap();
        assert_eq!(
            inner.sessions[inner.find_session_index(&session).unwrap()]
                .runtime
                .runtime_token(),
            token
        );
    }
    fixture.state.shutdown_persist_blocking();
    let restored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(restored.test_run_observations[&id], after);
    assert!(restored.test_run_observations.contains_key(&other));
}

#[tokio::test]
async fn monitoring_interruption_refuses_changed_origin_invalid_reason_and_terminal_rows() {
    let fixture = RunFixture::new("monitoring-refusals");
    let session = running_owner(&fixture, "monitoring-refusal-runtime");
    let id = start_observation(&fixture, &session, "terminal");
    let valid = request_for(&fixture, &id);
    let before = fixture.state.inner.lock().unwrap().test_run_observations[&id].clone();
    for (field, value, status) in [
        (
            "ownerSessionId",
            json!("a-different-owner"),
            StatusCode::CONFLICT,
        ),
        (
            "startedAt",
            json!("a-different-start"),
            StatusCode::CONFLICT,
        ),
        ("reason", json!("  "), StatusCode::BAD_REQUEST),
        ("reason", json!("a".repeat(1025)), StatusCode::BAD_REQUEST),
        ("extra", json!(true), StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let mut body = valid.clone();
        body[field] = value;
        let (actual, response) = interrupt(&fixture, &id, body).await;
        assert_eq!(actual, status, "{field}: {response}");
        if field == "extra" {
            assert!(
                response["error"]
                    .as_str()
                    .unwrap()
                    .contains("unknown field `extra`"),
                "the JSON boundary rejects the named unknown field: {response}"
            );
        }
        assert_eq!(
            fixture.state.inner.lock().unwrap().test_run_observations[&id],
            before
        );
    }
    assert_eq!(
        interrupt(&fixture, "observed-missing", valid.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    fixture.state.observe_test_command_end(
        &session,
        &EngramObservationProvenance::Ambient,
        "terminal",
        Some(EngramCommandExit::Code(101)),
    );
    let terminal = fixture.state.inner.lock().unwrap().test_run_observations[&id].clone();
    assert_eq!(
        interrupt(&fixture, &id, valid).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_observations[&id],
        terminal
    );
}

#[tokio::test]
async fn monitoring_interruption_refuses_registered_runs_and_correlated_aliases() {
    let fixture = RunFixture::new("monitoring-registered");
    let session = running_owner(&fixture, "monitoring-registered-runtime");
    let command = "node scripts/test-launcher.mjs full";
    let observed = start_command(&fixture, &session, "launcher-alias", command);
    let id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(
        &fs::canonicalize(&fixture.root).unwrap(),
        &test_run_timestamp(),
    );
    request["owner"] = json!(session);
    request["creatorPid"] = json!(std::process::id());
    let directory = write_run(
        &fixture.runs_dir(),
        &id,
        request,
        Some(running_results(Some(717))),
    );
    fixture
        .state
        .register_test_run(&session, registration_request(&id, &directory))
        .unwrap();
    let before = fixture.summary(&id);
    assert_eq!(
        interrupt(&fixture, &id, request_for(&fixture, &observed))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(fixture.summary(&id), before);
    // The production locator parser owns correlation, not a timestamp match.
    fixture.state.observe_test_command_artifact(
        &session,
        &EngramObservationProvenance::Ambient,
        "launcher-alias",
        command,
        &format!("RUN {}", test_run_display_path(&directory)),
        false,
    );
    assert!(
        fixture.state.inner.lock().unwrap().test_run_observations[&observed]
            .artifact
            .is_some()
    );
    let registered = fixture.summary(&id);
    let original = fixture.state.inner.lock().unwrap().test_run_observations[&observed].clone();
    assert_eq!(
        interrupt(&fixture, &observed, request_for(&fixture, &observed))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_observations[&observed],
        original
    );
    assert_eq!(fixture.summary(&id), registered);
    let discovered = format!("test-{}", Uuid::new_v4());
    write_run(
        &fixture.runs_dir(),
        &discovered,
        full_request(&fixture.root, &test_run_timestamp()),
        Some(passed_results()),
    );
    fixture.refresh(&[717]);
    assert_eq!(
        interrupt(&fixture, &discovered, request_for(&fixture, &observed))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn monitoring_interruption_never_acknowledges_failed_persistence() {
    let mut fixture = RunFixture::new("monitoring-write-failure");
    let session = running_owner(&fixture, "monitoring-failure-runtime");
    let id = start_observation(&fixture, &session, "failure");
    let request = request_for(&fixture, &id);
    // A directory cannot serve as the SQLite file. The real write path fails.
    fixture.state.persistence_path = Arc::new(fixture.root.clone());
    let (status, _) = interrupt(&fixture, &id, request).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.test_run_observations[&id].summary.interrupted);
    assert!(inner.test_runs.observations_failed.contains(&id));
    assert!(!inner.test_runs.observations_durable.contains(&id));
}
