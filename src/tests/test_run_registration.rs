// Test-only extension beside src/tests/test_runs.rs, mounted as its child.
// Owns durable registration and provider-observation regression witnesses.
// Uses the parent's artifact fixtures; does not execute a launcher or mint
// verification credit from an execution result.
use super::*;

#[path = "test_run_registration_identity.rs"]
mod identity;

#[path = "test_run_exact_query.rs"]
mod exact_query;

#[path = "test_run_wait_ownership.rs"]
mod wait_ownership;

#[path = "test_run_observation_review.rs"]
mod observation_review;

#[path = "test_run_final_unbound.rs"]
mod final_unbound;

#[path = "test_run_compaction.rs"]
mod compaction;

#[path = "test_run_observed_terminal.rs"]
mod observed_terminal;

#[path = "test_run_lifecycle_batch.rs"]
mod lifecycle_batch;

#[path = "test_run_batch_failure_deltas.rs"]
mod batch_failure_deltas;

#[tokio::test]
async fn http_registration_returns_the_versioned_durable_contract_and_one_global_row_on_retry() {
    let fixture = RunFixture::new("registration-http-contract");
    let session = owner(&fixture);
    let run_id = "test-http-registration";
    let directory = registered_run(&fixture, &session, run_id);
    let app = app_router(fixture.state.clone());
    for _ in 0..2 {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{session}/test-runs"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "runId": run_id, "runDirectory": test_run_display_path(&directory) })
                    .to_string(),
            ))
            .unwrap();
        let (status, ack): (StatusCode, Value) = request_json(&app, request).await;
        assert_eq!(status, StatusCode::CREATED, "{ack}");
        assert_eq!(
            ack,
            json!({ "contract": "termal-durable-test-run-registration", "version": 1, "runId": run_id, "registered": true })
        );
        let (status, list): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .uri("/api/test-runs")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["runs"].as_array().unwrap().len(), 1);
        assert_eq!(list["runs"][0]["registrationState"], "registered");
        assert_eq!(list["runs"][0]["trackingDurability"], "durable");
    }
}

#[test]
fn a_foreign_claude_turn_never_inherits_the_successors_work_binding_or_grant() {
    let fixture = RunFixture::new("registration-foreign-turn");
    let session = running_owner(&fixture, "registry-foreign-turn-runtime");
    let token = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.active_turn_generation = 4;
        record.engram.active_grant_id = Some("successor-grant".to_owned());
        record.engram.work_binding = Some(EngramControlWorkBinding {
            root_execution_id: "successor-root".to_owned(),
            work_id: "successor-work".to_owned(),
            run_id: "successor-run".to_owned(),
            work_revision: 1,
            claim_id: "successor-claim".to_owned(),
            claim_fence: 1,
        });
        record.runtime.runtime_token().unwrap()
    };
    let provenance = EngramObservationProvenance::Claude(ClaudeObservationProvenance {
        token,
        origin: ClaudeWorkOrigin::Attempt { turn_generation: 3 },
    });
    fixture.state.note_engram_command_started(
        &session,
        &provenance,
        "foreign-turn-tool",
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    let runs = fixture.summaries();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].registration.bound_work_ids.is_empty());
    assert_eq!(runs[0].registration.restart_would_void_evidence, None);
    let inner = fixture.state.inner.lock().unwrap();
    let row = inner.test_run_observations.values().next().unwrap();
    assert_eq!(row.turn_generation, 3);
    assert!(row.grant_identity.is_none());
    assert!(row.work_binding.is_none());
    assert!(row.source_root.is_none());
}

#[test]
fn explicit_launcher_locator_merges_running_visibility_in_api_and_sse_and_restores_on_loss() {
    let fixture = RunFixture::new("registration-exact-artifact");
    let session = running_owner(&fixture, "registry-artifact-runtime");
    let command = "node scripts/test-launcher.mjs full";
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "artifact-tool",
        Some(command),
        Some(&fixture.root.to_string_lossy()),
    );
    let observed_id = fixture.summaries()[0].run_id.clone();
    let run_id = format!("test-{}", Uuid::new_v4());
    let root = fs::canonicalize(&fixture.root).unwrap();
    let started = engram_command_started_at();
    assert!(
        engram_parse_time(&started).is_some(),
        "launcher request timestamp must be RFC 3339"
    );
    let mut request = full_request(&root, &started);
    request["owner"] = json!(session);
    request["creatorPid"] = json!(std::process::id());
    let directory = write_run(&fixture.runs_dir(), &run_id, request, None);
    let mut receiver = fixture.state.subscribe_delta_events();
    fixture.state.observe_test_command_artifact(
        &session,
        &EngramObservationProvenance::Ambient,
        "artifact-tool",
        command,
        &format!("STARTED {}", test_run_display_path(&directory)),
        false,
    );
    fixture.refresh(&[std::process::id()]);
    let runs = fixture.summaries();
    assert_eq!(
        runs.len(),
        1,
        "an explicit locator replaces the provider-only row"
    );
    assert_eq!(runs[0].run_id, run_id);
    assert_eq!(
        runs[0].state,
        TestRunState::Unknown,
        "request/PID alone cannot invent execution status"
    );
    let mut running = running_results(Some(std::process::id()));
    running["runId"] = json!(run_id);
    fs::write(directory.join("results.json"), running.to_string()).unwrap();
    fixture.refresh(&[std::process::id()]);
    assert_eq!(
        fixture.summary(&run_id).state,
        TestRunState::Running,
        "the existing results-plus-responsible-process boundary supplies the positive witness"
    );
    assert!(
        runs[0]
            .registration
            .tracking_provenances
            .contains(&"hostObserved".to_owned())
    );
    assert_eq!(
        runs[0].registration.superseded_run_ids,
        [observed_id.clone()]
    );
    let events = drain_test_run_deltas(&mut receiver);
    assert!(events.iter().any(
        |event| matches!(event, DeltaEvent::TestRunChanged { run, .. }
        if run.run_id == run_id && run.registration.superseded_run_ids == [observed_id.clone()])
    ));
    assert!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .values()
            .all(|row| row.evidence.is_empty())
    );
    // Remove only this fixture's request. Loss of the exact immutable identity
    // must restore the original durable observation, not silently hide it.
    fs::remove_file(directory.join("request.json")).unwrap();
    fs::remove_file(directory.join("results.json")).unwrap();
    fs::remove_dir(&directory).unwrap();
    let events = Mutex::new(Vec::<DeltaEvent>::new());
    fixture
        .state
        .refresh_test_runs_with(&|_, _| true, &|_, event| {
            events.lock().unwrap().push(event.clone())
        });
    assert!(
        fixture
            .summaries()
            .iter()
            .any(|run| run.run_id == observed_id)
    );
    assert!(
        events
            .into_inner()
            .unwrap()
            .iter()
            .any(|event| matches!(event,
        DeltaEvent::TestRunChanged { run, .. } if run.run_id == observed_id))
    );
}

#[test]
fn ambiguous_or_cut_launcher_output_never_suppresses_an_observed_execution() {
    let fixture = RunFixture::new("registration-artifact-refusal");
    let session = running_owner(&fixture, "registry-artifact-refusal-runtime");
    let command = "node scripts/test-launcher.mjs full";
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "artifact-refusal-tool",
        Some(command),
        Some(&fixture.root.to_string_lossy()),
    );
    let run_id = format!("test-{}", Uuid::new_v4());
    let mut request = full_request(
        &fs::canonicalize(&fixture.root).unwrap(),
        &engram_command_started_at(),
    );
    request["owner"] = json!(session);
    let directory = write_run(&fixture.runs_dir(), &run_id, request, None);
    let locator = format!("STARTED {}", test_run_display_path(&directory));
    for (output, cut) in [
        (locator.clone(), true),
        (format!("{locator}\nRUN {}", fixture.root.display()), false),
    ] {
        fixture.state.observe_test_command_artifact(
            &session,
            &EngramObservationProvenance::Ambient,
            "artifact-refusal-tool",
            command,
            &output,
            cut,
        );
        assert!(
            fixture
                .state
                .inner
                .lock()
                .unwrap()
                .test_run_observations
                .values()
                .all(|row| row.artifact.is_none())
        );
    }
}

fn owner(fixture: &RunFixture) -> String {
    create_test_project_session(
        &fixture.state,
        Agent::Codex,
        &fixture.project_id,
        &fixture.root,
    )
}

fn registered_run(fixture: &RunFixture, session: &str, id: &str) -> PathBuf {
    let mut request = full_request(&fixture.root, "2026-10-14T01:00:00.000Z");
    request["owner"] = json!(session);
    request["creatorPid"] = json!(std::process::id());
    request["detached"] = json!(true);
    write_run(&fixture.runs_dir(), id, request, None)
}

fn registration_request(id: &str, directory: &FsPath) -> RegisterTestRunRequest {
    RegisterTestRunRequest {
        run_id: id.to_owned(),
        run_directory: test_run_display_path(directory),
    }
}

#[test]
fn duplicate_and_lost_registration_ack_retain_one_durable_queryable_identity() {
    let fixture = RunFixture::new("registration-idempotent");
    let session = owner(&fixture);
    let directory = registered_run(&fixture, &session, "test-registration-idempotent");
    for _ in 0..2 {
        let ack = fixture
            .state
            .register_test_run(
                &session,
                registration_request("test-registration-idempotent", &directory),
            )
            .unwrap();
        assert_eq!(ack.contract, "termal-durable-test-run-registration");
        assert_eq!(ack.version, 1);
        assert!(ack.registered);
        assert_eq!(fixture.summaries().len(), 1);
        assert_eq!(
            fixture
                .summary(&ack.run_id)
                .registration
                .registration_state
                .as_deref(),
            Some("registered")
        );
    }
    let restored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(restored.test_run_registrations.len(), 1);
    assert_eq!(
        restored
            .test_run_registrations
            .values()
            .next()
            .unwrap()
            .stage_names,
        ["cargo-check", "rust-tests"]
    );
    let summaries = test_run_summaries_locked(&restored);
    assert_eq!(
        summaries.len(),
        1,
        "registration is visible before a cold index scan"
    );
    assert!(
        summaries[0].stages.is_empty(),
        "artifact loss cannot invent unrun stage verdicts"
    );
    assert_eq!(
        summaries[0].registration.planned_stage_names,
        ["cargo-check", "rust-tests"]
    );
    assert_eq!(
        summaries[0].registration.restart_would_void_evidence, None,
        "boot-loaded mutable correlation is not safe"
    );
    assert!(
        summaries[0]
            .registration
            .monitoring_interrupted_at
            .is_some()
    );
}

#[test]
fn immutable_registration_conflict_is_refused_without_replacing_the_original() {
    let fixture = RunFixture::new("registration-conflict");
    let session = owner(&fixture);
    let directory = registered_run(&fixture, &session, "test-registration-conflict");
    fixture
        .state
        .register_test_run(
            &session,
            registration_request("test-registration-conflict", &directory),
        )
        .unwrap();
    let original =
        fixture.state.inner.lock().unwrap().test_run_registrations["test-registration-conflict"]
            .clone();
    let path = directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["stages"] = json!([{ "name": "another-stage" }]);
    fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
    assert!(
        fixture
            .state
            .register_test_run(
                &session,
                registration_request("test-registration-conflict", &directory)
            )
            .is_err()
    );
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_registrations["test-registration-conflict"],
        original
    );
}

#[test]
fn wrong_owner_and_oversized_registration_requests_receive_no_durable_ack() {
    let fixture = RunFixture::new("registration-invalid");
    let session = owner(&fixture);
    let other = owner(&fixture);
    let directory = registered_run(&fixture, &session, "test-registration-invalid");
    assert!(
        fixture
            .state
            .register_test_run(
                &other,
                registration_request("test-registration-invalid", &directory)
            )
            .is_err()
    );
    fs::write(
        directory.join("request.json"),
        vec![b' '; TEST_RUN_JSON_MAX_BYTES as usize + 1],
    )
    .unwrap();
    assert!(
        fixture
            .state
            .register_test_run(
                &session,
                registration_request("test-registration-invalid", &directory)
            )
            .is_err()
    );
    assert!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .is_empty()
    );
}

#[test]
fn registration_persistence_failure_is_not_acknowledged_as_durable() {
    let mut fixture = RunFixture::new("registration-write-failure");
    let session = owner(&fixture);
    let directory = registered_run(&fixture, &session, "test-registration-write-failure");
    let failing_path = fixture.root.join("not-a-state-file");
    fs::create_dir(&failing_path).unwrap();
    fixture.state.persistence_path = Arc::new(failing_path);
    let Err(error) = fixture.state.register_test_run(
        &session,
        registration_request("test-registration-write-failure", &directory),
    ) else {
        panic!("failed persistence must not acknowledge registration");
    };
    assert!(
        error.message.contains("phase=persist_enqueue"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("category=persist_write_failed"),
        "{}",
        error.message
    );
    for detail in [
        "runId=test-registration-write-failure",
        "attemptId=",
        "stateInnerWaitMs=",
        "stateInnerHoldMs=",
    ] {
        assert!(error.message.contains(detail), "{}", error.message);
    }
    let summary = fixture.summary("test-registration-write-failure");
    assert_eq!(
        summary.registration.registration_state.as_deref(),
        Some("registrationPending")
    );
    assert_eq!(
        summary.registration.tracking_durability.as_deref(),
        Some("failed")
    );
    assert_eq!(summary.registration.restart_would_void_evidence, None);
}

fn running_owner(fixture: &RunFixture, runtime_id: &str) -> String {
    let session = owner(fixture);
    let (runtime, _commands) = test_codex_runtime_handle(runtime_id);
    let mut inner = fixture.state.inner.lock().unwrap();
    let index = inner.find_session_index(&session).unwrap();
    inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    session
}

#[test]
fn recognised_provider_start_is_global_before_its_result_and_survives_idle_and_boot() {
    let fixture = RunFixture::new("registration-observed-start");
    let session = running_owner(&fixture, "registry-runtime-one");
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "tool-one",
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    let summaries = fixture.summaries();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].state, TestRunState::Running);
    assert_eq!(
        summaries[0].registration.registration_state.as_deref(),
        Some("hostObserved")
    );
    assert_eq!(
        summaries[0].registration.tracking_durability.as_deref(),
        Some("durable")
    );
    assert!(summaries[0].registration.observed_at.is_some());
    assert!(
        summaries[0].run_dir.is_empty(),
        "no fabricated artifact path"
    );
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .session
            .status,
        SessionStatus::Idle
    );
    drop(inner);
    let restored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let loaded = test_run_summaries_locked(&restored);
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].state, TestRunState::Unknown);
    assert!(loaded[0].interrupted);
    assert_eq!(loaded[0].registration.restart_would_void_evidence, None);
}

#[test]
fn provider_native_pass_does_not_create_a_verification_record() {
    let fixture = RunFixture::new("registration-native-pass");
    let session = running_owner(&fixture, "registry-runtime-two");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.work_binding = Some(EngramControlWorkBinding {
            root_execution_id: "root-one".to_owned(),
            work_id: "work-one".to_owned(),
            run_id: "run-one".to_owned(),
            work_revision: 1,
            claim_id: "claim-one".to_owned(),
            claim_fence: 1,
        });
    }
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "tool-two",
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    fixture.state.observe_test_command_end(
        &session,
        &EngramObservationProvenance::Ambient,
        "tool-two",
        Some(EngramCommandExit::Code(0)),
    );
    let summaries = fixture.summaries();
    assert_eq!(summaries[0].state, TestRunState::Passed);
    assert_eq!(summaries[0].registration.restart_would_void_evidence, None);
    assert!(summaries[0].registration.verification_record_ids.is_empty());
    assert!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .values()
            .all(|row| row.evidence.is_empty())
    );
}

#[test]
fn reused_provider_tool_key_under_a_successor_runtime_cannot_finish_the_old_execution() {
    let fixture = RunFixture::new("registration-runtime-scope");
    let session = running_owner(&fixture, "registry-old-runtime");
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "reused-key",
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    let (runtime, _commands) = test_codex_runtime_handle("registry-new-runtime");
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    fixture.state.step_test_command_observation_persistence();
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "reused-key",
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    fixture.state.observe_test_command_end(
        &session,
        &EngramObservationProvenance::Ambient,
        "reused-key",
        Some(EngramCommandExit::Code(0)),
    );
    let runs = fixture.summaries();
    assert_eq!(runs.len(), 2);
    assert_eq!(
        runs.iter()
            .filter(|run| run.state == TestRunState::Passed)
            .count(),
        1
    );
    assert_eq!(
        runs.iter()
            .filter(|run| run.state == TestRunState::Unknown && run.interrupted)
            .count(),
        1
    );
}

#[test]
fn provider_observation_persistence_failure_remains_visible_and_uncertain() {
    let mut fixture = RunFixture::new("registration-observation-failure");
    let session = running_owner(&fixture, "registry-failed-persistence-runtime");
    let failing_path = fixture.root.join("not-a-state-file");
    fs::create_dir(&failing_path).unwrap();
    fixture.state.persistence_path = Arc::new(failing_path);
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "tool-failure",
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    let summary = fixture.summaries().pop().unwrap();
    assert_eq!(summary.state, TestRunState::Running);
    assert_eq!(
        summary.registration.tracking_durability.as_deref(),
        Some("failed")
    );
    assert_eq!(summary.registration.restart_would_void_evidence, None);
}

fn evidence_fixture(
    label: &str,
) -> (
    RunFixture,
    String,
    EngramAuthorityStoreKey,
    EngramControlWorkBinding,
    EngramTurnReport,
) {
    let fixture = RunFixture::new(label);
    let session = running_owner(&fixture, "registry-evidence-runtime");
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "evidence-tool",
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    fixture.state.observe_test_command_end(
        &session,
        &EngramObservationProvenance::Ambient,
        "evidence-tool",
        Some(EngramCommandExit::Code(0)),
    );
    let store = EngramAuthorityStoreKey {
        database_path: fixture.root.join("tracker.db"),
        project_id: "canonical-project".to_owned(),
    };
    let binding = EngramControlWorkBinding {
        root_execution_id: "root-execution-one".to_owned(),
        work_id: "work-one".to_owned(),
        run_id: "run-one".to_owned(),
        work_revision: 1,
        claim_id: "claim-one".to_owned(),
        claim_fence: 1,
    };
    let basis = EngramExecutionSourceBasis {
        workspace_id: fixture.root.to_string_lossy().into_owned(),
        source_revision: "a".repeat(64),
        source_root_generation: Some(1),
        source_root_state: None,
    };
    let observation_id = "b".repeat(64);
    let report = EngramTurnReport {
        observations: vec![EngramExecutionObservationInput {
            observation_id: observation_id.clone(),
            action_fingerprint: "c".repeat(64),
            effect: EngramEffect::Observe,
            outcome: EngramExecutionOutcome::Succeeded,
            source_changed: false,
            source_basis: Some(basis.clone()),
            observed_at: Some(stamp_now()),
        }],
        verification_evidence: vec![EngramVerificationEvidenceInput {
            producer_observation: EngramObservationReference::ObservationId {
                observation_id: observation_id.clone(),
            },
            check_kind: EngramVerificationKind::Test,
            environment: None,
            summary: None,
            refs: Vec::new(),
        }],
        ..EngramTurnReport::default()
    };
    let id = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let row = inner.test_run_observations.values_mut().next().unwrap();
        row.validated_unbound = false;
        row.work_binding = Some(binding.clone());
        row.evidence.push(TestRunEvidenceAssociation {
            store: store.clone(),
            binding: binding.clone(),
            source_basis: basis,
            producer_observation_id: observation_id,
            reported_outcome: EngramExecutionOutcome::Succeeded,
            action_fingerprint: "c".repeat(64),
            verification_record_id: None,
            checkpoint_id: None,
            confirmed_cursor: None,
            lifecycle_read: None,
        });
        row.execution_id.clone()
    };
    (fixture, id, store, binding, report)
}

fn exact_receipt() -> EngramCheckpointReceipt {
    serde_json::from_value(
        json!({ "grant_id": "receipt-grant", "checkpoint": "d".repeat(64),
        "cursor": 3, "confirmed_cursor": 3, "verification_evidence": ["e".repeat(64)] }),
    )
    .unwrap()
}

fn completed_lifecycle(
    fixture: &RunFixture,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
) -> Value {
    json!({
        "project_id": store.project_id, "work_id": binding.work_id, "root_execution_id": binding.root_execution_id,
        "run_id": binding.run_id, "claim_id": binding.claim_id, "run": {"state": "completed", "generation": 1},
        "named_root": {"state": "none"},
        "latest_event": {"event": "f".repeat(64), "position": {"feed": {"kind": "run_execution", "id": binding.run_id}, "position": 2},
            "generation": 1, "kind": "ended", "workspace_id": fixture.root.to_string_lossy(), "named_at": "2026-10-14T01:00:00Z"},
        "read_cut": {"feed": {"kind": "run_execution", "id": binding.run_id}, "position": 3},
    })
}

#[test]
fn exact_verification_receipt_and_completed_lifecycle_clear_only_their_association() {
    let (fixture, id, store, binding, report) = evidence_fixture("registration-exact-evidence");
    assert_eq!(
        fixture
            .summary(&id)
            .registration
            .restart_would_void_evidence,
        Some(true)
    );
    fixture
        .state
        .settle_test_run_verification_receipt(&report, &exact_receipt(), &store, &binding);
    let recorded = fixture.summary(&id);
    assert_eq!(
        recorded.registration.verification_record_ids,
        vec!["e".repeat(64)]
    );
    assert_eq!(
        recorded.registration.restart_would_void_evidence,
        Some(true),
        "recorded PASS remains at risk before work completion"
    );
    let read: EngramNamedRootReadResponse =
        serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    assert_eq!(
        fixture
            .summary(&id)
            .registration
            .restart_would_void_evidence,
        Some(false)
    );

    let restored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    *fixture.state.inner.lock().unwrap() = restored;
    assert_eq!(
        fixture
            .summary(&id)
            .registration
            .restart_would_void_evidence,
        None,
        "loaded completion must be revalidated"
    );
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    assert_eq!(
        fixture
            .summary(&id)
            .registration
            .restart_would_void_evidence,
        Some(false)
    );
}

#[test]
fn cancelled_released_wrong_lifecycle_or_generation_never_clear_recorded_passing_evidence() {
    for case in [
        "cancelled",
        "released",
        "other-run",
        "other-root",
        "other-generation",
        "bad-cut",
    ] {
        let (fixture, id, store, binding, report) =
            evidence_fixture(&format!("registration-negative-{case}"));
        fixture.state.settle_test_run_verification_receipt(
            &report,
            &exact_receipt(),
            &store,
            &binding,
        );
        let mut proof = completed_lifecycle(&fixture, &store, &binding);
        let mut read_binding = binding.clone();
        match case {
            "cancelled" => proof["run"]["state"] = json!("cancelled"),
            "released" => {
                proof["run"]["state"] = json!("active");
                proof["named_root"] = json!({"state": "unbound_by_release", "last_generation": 1, "released_at_position": 3});
            }
            "other-run" => {
                read_binding.run_id = "other-run".to_owned();
                proof["run_id"] = json!(read_binding.run_id);
                proof["latest_event"]["position"]["feed"]["id"] = json!(read_binding.run_id);
                proof["read_cut"]["feed"]["id"] = json!(read_binding.run_id);
            }
            "other-root" => {
                read_binding.root_execution_id = "another-root".to_owned();
                proof["root_execution_id"] = json!(read_binding.root_execution_id);
            }
            "other-generation" => proof["latest_event"]["generation"] = json!(2),
            "bad-cut" => proof["read_cut"]["position"] = json!(1),
            _ => unreachable!(),
        }
        let read = serde_json::from_value(proof).unwrap();
        fixture
            .state
            .revalidate_test_run_lifecycle(&store, &read_binding, &read);
        assert_eq!(
            fixture
                .summary(&id)
                .registration
                .restart_would_void_evidence,
            Some(true),
            "{case}"
        );
    }
}

#[test]
fn partial_or_malformed_checkpoint_receipts_do_not_settle_any_evidence() {
    for records in [
        Vec::new(),
        vec!["not-an-object-id".to_owned()],
        vec!["e".repeat(64), "f".repeat(64)],
    ] {
        let (fixture, id, store, binding, report) =
            evidence_fixture("registration-partial-receipt");
        let mut receipt = exact_receipt();
        receipt.verification_evidence = records;
        fixture
            .state
            .settle_test_run_verification_receipt(&report, &receipt, &store, &binding);
        let summary = fixture.summary(&id);
        assert!(summary.registration.verification_record_ids.is_empty());
        assert_eq!(summary.registration.restart_would_void_evidence, Some(true));
    }
}

#[test]
fn receipt_from_another_store_binding_source_or_action_never_settles_the_original_check() {
    for case in ["store", "claim", "work", "root", "run", "action", "source"] {
        let (fixture, id, store, binding, mut report) =
            evidence_fixture(&format!("registration-receipt-scope-{case}"));
        let mut receipt_store = store.clone();
        let mut receipt_binding = binding.clone();
        match case {
            "store" => receipt_store.project_id = "other-project".to_owned(),
            "claim" => receipt_binding.claim_id = "other-claim".to_owned(),
            "work" => receipt_binding.work_id = "other-work".to_owned(),
            "root" => receipt_binding.root_execution_id = "other-root".to_owned(),
            "run" => receipt_binding.run_id = "other-run".to_owned(),
            "action" => report.observations[0].action_fingerprint = "a".repeat(64),
            "source" => {
                report.observations[0]
                    .source_basis
                    .as_mut()
                    .unwrap()
                    .source_revision = "f".repeat(64)
            }
            _ => unreachable!(),
        }
        fixture.state.settle_test_run_verification_receipt(
            &report,
            &exact_receipt(),
            &receipt_store,
            &receipt_binding,
        );
        assert!(
            fixture
                .summary(&id)
                .registration
                .verification_record_ids
                .is_empty(),
            "{case}"
        );
        assert_eq!(
            fixture
                .summary(&id)
                .registration
                .restart_would_void_evidence,
            Some(true),
            "{case}"
        );
    }
}

#[test]
fn every_associated_work_must_settle_and_true_dominates_unknown() {
    let (fixture, id, store, binding, report) = evidence_fixture("registration-all-associations");
    fixture
        .state
        .settle_test_run_verification_receipt(&report, &exact_receipt(), &store, &binding);
    let read = serde_json::from_value(completed_lifecycle(&fixture, &store, &binding)).unwrap();
    fixture
        .state
        .revalidate_test_run_lifecycle(&store, &binding, &read);
    assert_eq!(
        fixture
            .summary(&id)
            .registration
            .restart_would_void_evidence,
        Some(false)
    );
    let mut inner = fixture.state.inner.lock().unwrap();
    let row = inner.test_run_observations.get_mut(&id).unwrap();
    let mut second = row.evidence[0].clone();
    second.producer_observation_id = "2".repeat(64);
    second.binding.work_id = "second-work".to_owned();
    second.binding.run_id = "second-run".to_owned();
    second.verification_record_id = None;
    second.checkpoint_id = None;
    second.lifecycle_read = None;
    row.evidence.push(second);
    drop(inner);
    let summary = fixture.summary(&id);
    assert_eq!(
        summary.registration.bound_work_ids,
        ["second-work", "work-one"]
    );
    assert_eq!(
        summary.registration.restart_would_void_evidence,
        Some(true),
        "first lifecycle completion cannot discharge another check"
    );
    let mut inner = fixture.state.inner.lock().unwrap();
    let row = inner.test_run_observations.get_mut(&id).unwrap();
    row.evidence[1].verification_record_id = Some("3".repeat(64));
    row.evidence[1].checkpoint_id = Some("4".repeat(64));
    row.evidence[1].reported_outcome = EngramExecutionOutcome::Failed;
    drop(inner);
    assert_eq!(
        fixture
            .summary(&id)
            .registration
            .restart_would_void_evidence,
        Some(false),
        "recorded failure has no passing lifecycle to preserve"
    );
    let mut inner = fixture.state.inner.lock().unwrap();
    inner
        .test_run_observations
        .get_mut(&id)
        .unwrap()
        .monitoring_interrupted_at = Some(engram_command_started_at());
    drop(inner);
    assert_eq!(
        fixture
            .summary(&id)
            .registration
            .restart_would_void_evidence,
        None,
        "unrevalidated boot association remains unknown"
    );
}

#[test]
fn positively_unbound_terminal_executions_still_obey_the_ordinary_retention_cap() {
    let fixture = RunFixture::new("registration-safe-retention");
    let session = running_owner(&fixture, "registry-safe-retention-runtime");
    for index in 0..TEST_RUN_TERMINAL_RUNS_PER_PROJECT + 2 {
        let key = format!("retention-tool-{index}");
        fixture.state.note_engram_command_started(
            &session,
            &EngramObservationProvenance::Ambient,
            &key,
            Some("cargo test"),
            Some(&fixture.root.to_string_lossy()),
        );
        fixture.state.observe_test_command_end(
            &session,
            &EngramObservationProvenance::Ambient,
            &key,
            Some(EngramCommandExit::Code(0)),
        );
    }
    let runs = fixture.summaries();
    assert_eq!(runs.len(), TEST_RUN_TERMINAL_RUNS_PER_PROJECT);
    assert!(runs.iter().all(|run| run.state == TestRunState::Passed
        && run.registration.restart_would_void_evidence == Some(false)));
    assert_eq!(
        fixture
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .len(),
        TEST_RUN_TERMINAL_RUNS_PER_PROJECT,
        "resolved full detail is bounded"
    );
    let inner = fixture.state.inner.lock().unwrap();
    assert_eq!(inner.test_run_compacted.len(), 2);
    assert_eq!(inner.test_run_compacted.values().map(|row| row.observations.len()).sum::<usize>(), 2,
        "every omitted execution retains exact durable identity and proof");
}

#[test]
fn late_command_description_keeps_the_original_start_owner_without_retroactive_credit() {
    let fixture = RunFixture::new("registration-late-description");
    let session = running_owner(&fixture, "registry-late-description-runtime");
    let original = EngramControlWorkBinding {
        root_execution_id: "original-root".to_owned(),
        work_id: "original-work".to_owned(),
        run_id: "original-run".to_owned(),
        work_revision: 1,
        claim_id: "original-claim".to_owned(),
        claim_fence: 1,
    };
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.work_binding = Some(original.clone());
    }
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "description-tool",
        None,
        Some(&fixture.root.to_string_lossy()),
    );
    assert!(
        fixture.summaries().is_empty(),
        "unknown command text is not invented as a test"
    );
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index]
            .engram
            .work_binding
            .as_mut()
            .unwrap()
            .work_id = "successor-work".to_owned();
    }
    fixture.state.note_engram_command_described(
        &session,
        &EngramObservationProvenance::Ambient,
        "description-tool",
        Some("cargo test"),
        None,
    );
    let runs = fixture.summaries();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, TestRunState::Running);
    assert_eq!(runs[0].registration.bound_work_ids, [original.work_id]);
    let inner = fixture.state.inner.lock().unwrap();
    assert!(inner.test_runs.unclassified_commands.is_empty());
    assert!(
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .engram
            .active_turn_checks
            .is_empty(),
        "description cannot create a retroactive source capture/check"
    );
    drop(inner);
    fixture.state.observe_test_command_end(
        &session,
        &EngramObservationProvenance::Ambient,
        "description-tool",
        Some(EngramCommandExit::Code(0)),
    );
    assert!(
        fixture
            .summary(&runs[0].run_id)
            .registration
            .verification_record_ids
            .is_empty()
    );
}

#[test]
fn live_durability_delta_has_a_revision_after_the_preceding_persistence_snapshot() {
    let fixture = RunFixture::new("registration-durability-stream");
    let session = running_owner(&fixture, "registry-durability-stream-runtime");
    let mut receiver = fixture.state.subscribe_stream_events();
    fixture.state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "stream-tool",
        Some("cargo test"),
        Some(&fixture.root.to_string_lossy()),
    );
    let mut preceding_revision = 0;
    let mut saw_durable = false;
    while let Ok(event) = receiver.try_recv() {
        match event {
            SseStreamEvent::State(body) => {
                let state: Value = serde_json::from_str(&body).unwrap();
                preceding_revision = state["revision"].as_u64().unwrap();
            }
            SseStreamEvent::Delta(body) => {
                let delta: Value = serde_json::from_str(&body).unwrap();
                if delta["type"] == "testRunChanged"
                    && delta["run"]["trackingDurability"] == "durable"
                {
                    assert!(
                        delta["revision"].as_u64().unwrap() > preceding_revision,
                        "the client's revision gate must accept the durability update"
                    );
                    saw_durable = true;
                }
            }
            SseStreamEvent::Lagged => panic!("the bounded fixture must not lose events"),
            SseStreamEvent::LegacyState(_) | SseStreamEvent::StateDiff(_) => {
                panic!("a state without a broadcaster worker never diffs")
            }
        }
    }
    assert!(saw_durable);
}

#[test]
fn mixed_checkpoint_receipt_keeps_the_test_object_id_in_input_order() {
    let (fixture, id, store, binding, mut report) = evidence_fixture("registration-mixed-receipt");
    let mut build = report.verification_evidence[0].clone();
    build.check_kind = EngramVerificationKind::Build;
    report.verification_evidence.insert(0, build);
    let mut receipt = exact_receipt();
    receipt.verification_evidence = vec!["1".repeat(64), "2".repeat(64)];
    fixture
        .state
        .settle_test_run_verification_receipt(&report, &receipt, &store, &binding);
    assert_eq!(
        fixture.summary(&id).registration.verification_record_ids,
        ["2".repeat(64)],
        "a Build receipt in the same report cannot settle a Test association"
    );
}
