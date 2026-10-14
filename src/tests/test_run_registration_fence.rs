// Test-only child beside src/tests/persist_fence.rs, mounted there.
// Owns the connected-worker registration ACK boundary using real SQLite.
// Does not fake a fence completion or start a production persistence thread.
use super::*;

#[test]
fn registration_ack_waits_for_the_actual_background_sqlite_commit() {
    let mut fixture = FenceFixture::new();
    let mut state = test_app_state();
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
    state.persistence_path = Arc::new(fixture.root.path().join("termal.sqlite"));
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

#[test]
fn test_run_compaction_keeps_full_detail_until_its_exact_proof_reaches_sqlite() {
    let mut fixture = FenceFixture::new();
    let mut state = test_app_state();
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
    state.persistence_path = Arc::new(fixture.root.path().join("termal.sqlite"));
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
    assert!(
        delta.metadata.test_run_observations.contains_key(&oldest),
        "the exact proof is persisted while old detail is still retained"
    );
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
