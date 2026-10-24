// Owns the registration ACK boundary while an unrelated index scan is held.
// Does not simulate durability or assert a machine-dependent latency limit.
// Split from test_run_registration_fence.rs and uses its real SQLite worker.
use super::*;

#[test]
fn durable_registration_and_query_do_not_wait_for_an_unrelated_rescan() {
    let mut fixture = FenceFixture::new();
    let mut state = fixture.app_state();
    let root = fixture.root.path().join("registration-project");
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project = create_test_project(&state, &root, "Registration isolation");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    let runs = root.join(".git/review-runs");
    for ordinal in 0..128 {
        fs::create_dir_all(runs.join(format!("test-unrelated-{ordinal:04}"))).unwrap();
    }
    let run_id = "test-registration-with-held-rescan";
    let directory = runs.join(run_id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("request.json"),
        json!({
            "runId": run_id, "root": test_run_display_path(&root), "owner": session,
            "full": true, "stages": [{"name":"rust-tests"}],
            "started": engram_command_started_at(), "creatorPid": std::process::id()
        }).to_string(),
    ).unwrap();
    state.persist_tx = fixture.tx.clone();
    fixture.inner = state.inner.clone();
    let rescan = state.inner.lock().unwrap().test_runs.rescan.clone();
    let scan_guard = rescan.lock().unwrap();
    let (queued_tx, queued_rx) = mpsc::channel();
    let (reply_tx, reply_rx) = mpsc::channel();
    let worker = {
        let state = state.clone();
        let session = session.clone();
        let directory = directory.clone();
        std::thread::spawn(move || {
            TEST_TEST_RUN_REGISTRATION_FENCE_SENT.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || queued_tx.send(()).unwrap()));
            });
            reply_tx.send(state.register_test_run(&session, RegisterTestRunRequest {
                run_id: run_id.to_owned(),
                run_directory: test_run_display_path(&directory),
            })).unwrap();
        })
    };
    // Release the held scan and join even if a precondition or the ACK wait
    // fails. A RED must not strand its worker or fixture directory.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        queued_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        fixture.receive(false);
        assert!(!fixture.batch.drain(&fixture.rx, false));
        assert_eq!(fixture.batch.pending.len(), 1);
        let delta = fixture.collect();
        assert!(fixture.batch.pending[0].target.is_in_delta(&delta));
        assert!(matches!(reply_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "registration cannot ACK before the real durable write");
        fixture.write(&delta).unwrap();
        let started = std::time::Instant::now();
        let response = reply_rx.recv_timeout(phase_sync::DEADLOCK_GUARD);
        eprintln!("registration after durable commit with 128 unrelated directories and held scan: {:?}", started.elapsed());
        let ack = response.expect("durable registration ACK must not wait for an unrelated global rescan").unwrap();
        assert!(ack.registered);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2).enable_all().build().unwrap();
        let (status, detail): (StatusCode, Value) = runtime.block_on(request_json(
            &app_router(state.clone()),
            Request::builder().uri(format!("/api/test-runs/{run_id}"))
                .body(Body::empty()).unwrap(),
        ));
        assert_eq!(status, StatusCode::OK, "{detail}");
        assert_eq!(detail["run"]["runId"], run_id);
        assert_eq!(detail["run"]["registrationState"], "registered");
        assert_eq!(detail["run"]["trackingDurability"], "durable");
        assert_eq!(detail["run"]["ownerSessionId"], session);
        assert_eq!(detail["run"]["runDir"], test_run_display_path(&directory));
        assert_eq!(detail["source"], "registration");
        let restored = load_state(state.persistence_path.as_path()).unwrap().unwrap();
        assert!(restored.test_run_registrations.contains_key(run_id));
    }));
    drop(scan_guard);
    worker.join().expect("registration worker should finish after scan release");
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    state.refresh_test_runs();
    assert!(state.inner.lock().unwrap().test_runs.find(run_id).is_some(),
        "the ordinary scanner must catch up after its mutex is released");
}
