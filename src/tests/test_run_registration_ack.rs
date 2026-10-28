//! Registration response versus an unrelated holder after durable publication.
//! Uses the production background writer; barriers determine ordering, not time.
use super::*;

#[test]
fn published_registration_ack_does_not_reacquire_state_behind_an_unrelated_holder() {
    let _temp_scope = TestTempRootDirectoryScope::repository_local();
    let root = TestTempRoot::create("registration-ack-project");
    let store = TestTempRoot::create("registration-ack-store");
    run_git_test_command(&root, &["init", "--quiet"]);
    let state = AppState::new_with_paths(
        test_run_display_path(&root),
        store.join("termal.sqlite"),
        store.join("orchestrators.json"),
    )
    .unwrap();
    let project = create_test_project(&state, &root, "Registration acknowledgement");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    let run_id = "test-published-registration-ack";
    let directory = root.join(".git/review-runs").join(run_id);
    fs::create_dir_all(&directory).unwrap();
    let request = json!({
        "runId": run_id, "root": test_run_display_path(&root), "owner": session,
        "full": true, "stages": [{"name": "rust-tests"}],
        "started": engram_command_started_at(), "creatorPid": std::process::id()
    });
    fs::write(directory.join("request.json"), request.to_string()).unwrap();

    let (acquire_tx, acquire_rx) = mpsc::channel();
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let holder = {
        let state = state.clone();
        std::thread::spawn(move || {
            acquire_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
            let guard = state.inner.lock().unwrap();
            held_tx.send(()).unwrap();
            // Disconnect also releases the holder if the test unwinds.
            let _ = release_rx.recv_timeout(phase_sync::DEADLOCK_GUARD);
            drop(guard);
        })
    };
    let registration = {
        let state = state.clone();
        let session = session.clone();
        let directory = directory.clone();
        std::thread::spawn(move || {
            let lock_event = event_tx.clone();
            TEST_TEST_RUN_REGISTRATION_PUBLISHED.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || {
                    acquire_tx.send(()).unwrap();
                    held_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
                    // Arm only after another thread owns StateInner. A second
                    // acquisition is observed before it blocks, without sleeps.
                    TEST_TEST_RUN_REGISTRATION_NEXT_LOCK.with(|slot| {
                        *slot.borrow_mut() = Some(Box::new(move || {
                            lock_event.send("state lock attempted").unwrap();
                        }));
                    });
                }));
            });
            let result = state.register_test_run(&session, RegisterTestRunRequest {
                run_id: run_id.to_owned(),
                run_directory: test_run_display_path(&directory),
            });
            // Clear an unused hook on the fixed path, before thread teardown.
            let _ = TEST_TEST_RUN_REGISTRATION_NEXT_LOCK.with(|slot| slot.borrow_mut().take());
            let _ = event_tx.send("response returned");
            result
        })
    };

    // Complete teardown before any causal assertion, on both RED and GREEN.
    let first_event = event_rx.recv_timeout(phase_sync::DEADLOCK_GUARD);
    let _ = release_tx.send(());
    let holder_result = holder.join();
    let registration_result = registration.join();
    let original_registration = state.inner.lock().unwrap().test_run_registrations[run_id].clone();
    // Exercise replay and refusal against the same production writer, without
    // replacing the recorded operation or manufacturing a new identity.
    let replay = state.register_test_run(&session, RegisterTestRunRequest {
        run_id: run_id.to_owned(),
        run_directory: test_run_display_path(&directory),
    });
    let mut changed = request.clone();
    changed["stages"] = json!([{"name": "different-stage"}]);
    fs::write(directory.join("request.json"), changed.to_string()).unwrap();
    let changed_request = state.register_test_run(&session, RegisterTestRunRequest {
        run_id: run_id.to_owned(),
        run_directory: test_run_display_path(&directory),
    });
    fs::write(directory.join("request.json"), request.to_string()).unwrap();
    state.shutdown_persist_blocking();
    holder_result.unwrap();
    let ack = registration_result.unwrap().unwrap();
    assert!(ack.registered);
    assert_eq!(ack.run_id, run_id);
    let replay = replay.unwrap();
    assert!(replay.registered);
    assert_eq!(replay.run_id, run_id);
    assert_eq!(changed_request.err().expect("changed identity must refuse").status, StatusCode::CONFLICT);
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.test_run_registrations.len(), 1);
        assert_eq!(inner.test_run_registrations[run_id], original_registration);
    }
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.test_run_registrations.contains_key(run_id), "real SQLite ACK survives reload");
    assert!(!directory.join("results.json").exists(), "registration does not start a stage");
    assert!(loaded.test_run_observations.is_empty(), "registration supplies no execution evidence");
    assert_eq!(
        first_event.unwrap(), "response returned",
        "an acknowledged, published registration must return before an unrelated StateInner holder releases"
    );
}
