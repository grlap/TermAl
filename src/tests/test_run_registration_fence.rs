// Test-only child beside src/tests/persist_fence.rs, mounted there.
// Owns the connected-worker registration ACK boundary using real SQLite.
// Does not fake a fence completion or start a production persistence thread.
use super::*;

#[path = "test_run_observation_publication.rs"]
mod observation_publication;

#[path = "test_run_registration_latency.rs"]
mod latency;

#[path = "test_run_phase_trace.rs"]
mod phase_trace;

#[path = "test_run_registration_history.rs"]
mod history;

#[test]
fn registration_ack_waits_for_the_actual_background_sqlite_commit() {
    let mut fixture = FenceFixture::new();
    let mut state = fixture.app_state();
    let root = fixture.root.path().join("project");
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project = create_test_project(&state, &root, "Registration fence");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    let run_id = "test-connected-registration";
    let directory = root.join(".git/review-runs").join(run_id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("request.json"),
        json!({
            "runId": run_id, "root": test_run_display_path(&root), "owner": session,
            "full": true, "stages": [{"name":"rust-tests"}],
            "started": engram_command_started_at(), "creatorPid": std::process::id()
        })
        .to_string(),
    )
    .unwrap();
    state.persist_tx = fixture.tx.clone();
    fixture.inner = state.inner.clone();
    let request = RegisterTestRunRequest {
        run_id: run_id.to_owned(),
        run_directory: test_run_display_path(&directory),
    };
    let (ready_tx, ready_rx) = mpsc::channel();
    let (reply_tx, reply_rx) = mpsc::channel();
    let worker = {
        let state = state.clone();
        std::thread::spawn(move || {
            TEST_TEST_RUN_REGISTRATION_FENCE_SENT.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || ready_tx.send(()).unwrap()));
            });
            reply_tx
                .send(state.register_test_run(&session, request))
                .unwrap();
        })
    };
    ready_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    assert!(
        matches!(reply_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "enqueue is not a durable ACK"
    );
    assert!(
        !state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .registered
            .contains(run_id)
    );
    fixture.receive(false);
    // Production accepts one wake, then drains queued fences before collecting
    // its tick. The ready barrier proves this exact fence is already queued.
    assert!(!fixture.batch.drain(&fixture.rx, false));
    assert_eq!(fixture.batch.pending.len(), 1);
    let delta = fixture.collect();
    assert!(
        fixture.batch.pending[0].target.is_in_delta(&delta),
        "the actual collected registration content matches its queued fence"
    );
    assert!(
        matches!(reply_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "collection is not a durable ACK"
    );
    fixture.write(&delta).unwrap();
    let ack = reply_rx
        .recv_timeout(phase_sync::DEADLOCK_GUARD)
        .unwrap()
        .unwrap();
    worker.join().unwrap();
    assert!(ack.registered);
    assert_eq!(ack.version, 1);
    assert_eq!(state.test_run_summaries(None).len(), 1);
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        loaded.test_run_registrations.contains_key(run_id),
        "ACK survives real reload"
    );
}

#[tokio::test]
async fn registration_fence_failures_report_the_actual_phase_and_writer_progress() {
    for (phase, fault, category, progress) in [
        (
            "queued",
            PersistFenceError::Deadline,
            "deadline",
            "queued for the writer",
        ),
        (
            "accepted",
            PersistFenceError::WorkerStopped,
            "worker_stopped",
            "accepted, awaiting a writer tick",
        ),
        (
            "in-tick",
            PersistFenceError::WriteFailed("fixture write refused".to_owned()),
            "persist_write_failed",
            "in a writer tick",
        ),
        (
            "shutdown",
            PersistFenceError::Shutdown,
            "worker_shutdown",
            "accepted, awaiting a writer tick",
        ),
        (
            "superseded",
            PersistFenceError::Superseded,
            "superseded",
            "accepted, awaiting a writer tick",
        ),
    ] {
        let mut fixture = FenceFixture::new();
        let mut state = fixture.app_state();
        let root = fixture.root.path().join("registration-diagnostic-project");
        fs::create_dir_all(&root).unwrap();
        run_git_test_command(&root, &["init", "--quiet"]);
        let project = create_test_project(&state, &root, "Registration diagnostics");
        let session = create_test_project_session(&state, Agent::Codex, &project, &root);
        let run_id = format!("test-registration-diagnostic-{phase}");
        let directory = root.join(".git/review-runs").join(&run_id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("request.json"),
            json!({
                "runId": run_id, "root": test_run_display_path(&root), "owner": session,
                "full": true, "stages": [{"name":"rust-tests"}],
                "started": engram_command_started_at(), "creatorPid": std::process::id()
            })
            .to_string(),
        )
        .unwrap();
        state.persist_tx = fixture.tx.clone();
        fixture.inner = state.inner.clone();
        let request = RegisterTestRunRequest {
            run_id: run_id.clone(),
            run_directory: test_run_display_path(&directory),
        };
        let (ready_tx, ready_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        let worker = {
            let state = state.clone();
            let session = session.clone();
            std::thread::spawn(move || {
                TEST_TEST_RUN_REGISTRATION_FENCE_SENT.with(|slot| {
                    *slot.borrow_mut() = Some(Box::new(move || ready_tx.send(()).unwrap()));
                });
                reply_tx
                    .send(state.register_test_run(&session, request))
                    .unwrap();
            })
        };
        ready_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        fixture.receive(false);
        let PersistRequest::Fence(fence) =
            fixture.rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap()
        else {
            panic!("the actual registration fence must follow its Delta");
        };
        assert!(matches!(
            fence.target,
            PersistFenceTarget::TestRunRegistration(_)
        ));
        assert!(matches!(
            reply_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let deadline = matches!(&fault, PersistFenceError::Deadline);
        let exact_fence_error = format!("{fault:?}");
        if phase == "queued" {
            // Leave this exact fence unaccepted, as when an earlier tick owns
            // the writer. Resolve the boundary, not a wall-clock sleep.
            fence.finish(Err(fault));
        } else {
            fixture.batch.accept(PersistRequest::Fence(fence));
            if phase == "in-tick" {
                fixture.batch.mark_in_tick();
            }
            fixture.batch.pending[0].finish(Err(fault));
        }
        let Err(error) = reply_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap() else {
            panic!("a failed fence cannot acknowledge a registration");
        };
        worker.join().unwrap();
        assert!(
            !state
                .inner
                .lock()
                .unwrap()
                .test_runs
                .registered
                .contains(&run_id)
        );
        let response = error.into_response();
        assert_eq!(response.status(), if deadline {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        });
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        if deadline {
            assert_eq!(body["contract"], "termal-durability-pending");
            assert_eq!(body["version"], 1);
            assert_eq!(body["code"], "durability_pending");
            assert_eq!(body["outcome"], "pending");
            assert_eq!(body["operation"], "test_run_registration");
            assert_eq!(body["identity"]["runId"], run_id);
            assert_eq!(body["retry"]["mode"], "same_request");
        } else {
            assert!(body.get("contract").is_none(), "{body}");
        }
        let message = body["error"].as_str().unwrap();
        assert!(message.contains("phase=durability_wait"), "{message}");
        assert!(
            message.contains(&format!("category={category}")),
            "{message}"
        );
        assert!(
            message.contains(&format!("writerProgress={progress}")),
            "{message}"
        );
        assert!(
            message.contains("elapsedMs=") && message.contains("phaseElapsedMs="),
            "{message}"
        );
        assert!(message.contains(&format!("runId={run_id}")), "{message}");
        assert!(message.contains("attemptId="), "{message}");
        assert!(message.contains("stateInnerWaitMs="), "{message}");
        assert!(message.contains("stateInnerHoldMs="), "{message}");
        assert!(
            message.contains(&format!("fenceError={exact_fence_error}")),
            "{message}"
        );
        assert!(!message.contains('\n') && message.len() <= 2048);
    }
}

#[test]
fn test_run_compaction_keeps_full_detail_until_its_exact_proof_reaches_sqlite() {
    let mut fixture = FenceFixture::new();
    let mut state = fixture.app_state();
    let root = fixture.root.path().join("compaction-project");
    fs::create_dir(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project = create_test_project(&state, &root, "Compaction fence");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    let (runtime, _commands) = test_codex_runtime_handle("compaction-fence-runtime");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    for number in 0..TEST_RUN_TERMINAL_RUNS_PER_PROJECT {
        let key = format!("compaction-fence-{number}");
        state.note_engram_command_started(
            &session,
            &EngramObservationProvenance::Ambient,
            &key,
            Some("cargo test"),
            Some(&root.to_string_lossy()),
        );
        state.observe_test_command_end(
            &session,
            &EngramObservationProvenance::Ambient,
            &key,
            Some(EngramCommandExit::Code(0)),
        );
    }
    let oldest = {
        let inner = state.inner.lock().unwrap();
        inner
            .test_run_observations
            .values()
            .min_by(|a, b| {
                a.summary
                    .ended_at
                    .cmp(&b.summary.ended_at)
                    .then_with(|| b.execution_id.cmp(&a.execution_id))
            })
            .unwrap()
            .execution_id
            .clone()
    };
    state.note_engram_command_started(
        &session,
        &EngramObservationProvenance::Ambient,
        "compaction-fence-last",
        Some("cargo test"),
        Some(&root.to_string_lossy()),
    );
    state.persist_tx = fixture.tx.clone();
    fixture.inner = state.inner.clone();
    state.observe_test_command_end(
        &session,
        &EngramObservationProvenance::Ambient,
        "compaction-fence-last",
        Some(EngramCommandExit::Code(0)),
    );
    fixture.receive(false);
    assert!(!fixture.batch.drain(&fixture.rx, false));
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    state.step_test_command_observation_persistence();
    state.step_test_run_compaction();
    {
        let inner = state.inner.lock().unwrap();
        assert!(
            inner.test_run_observations.contains_key(&oldest),
            "enqueue cannot remove detail"
        );
        assert!(!inner.test_runs.compactions_durable.contains(&oldest));
    }
    fixture.receive(false);
    assert!(!fixture.batch.drain(&fixture.rx, false));
    let delta = fixture.collect();
    let fence = fixture
        .batch
        .pending
        .iter()
        .find(|fence| matches!(&fence.target, PersistFenceTarget::TestRunCompaction(_)))
        .unwrap();
    assert!(fence.target.is_in_delta(&delta));
    assert!(delta.metadata.metadata_collection_is_omitted("testRunObservations"));
    let retained = read_metadata_for_fence(fixture.cache.connection.as_ref().unwrap(), &["testRunObservations"])
        .unwrap().unwrap();
    assert_eq!(retained.test_run_observations.get(&oldest),
        state.inner.lock().unwrap().test_run_observations.get(&oldest),
        "unchanged old detail remains durable while the exact proof is selected");
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .test_run_observations
            .contains_key(&oldest),
        "collection is not durability"
    );
    fixture.write(&delta).unwrap();
    state.step_test_run_compaction();
    let inner = state.inner.lock().unwrap();
    assert!(!inner.test_run_observations.contains_key(&oldest));
    assert!(inner.test_runs.compactions_durable.contains(&oldest));
    assert_eq!(
        inner.test_run_compacted[&oldest].observations[0].execution_id,
        oldest
    );
    drop(inner);
    let stored = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        stored.test_run_compacted.contains_key(&oldest),
        "SQLite retains the identity/proof before detail removal"
    );
}
