//! A deadline after enqueue is pending, not proof of a rejected operation.
//! Drive the real registration and SQLite writer at explicit phase boundaries.
use super::*;

#[tokio::test]
async fn durability_pending_wire_requires_a_typed_deadline_and_an_operation_boundary() {
    for error in [
        ApiError::internal("Deadline text alone is not typed evidence"),
        ApiError::bad_gateway("the writer stopped"),
        ApiError::conflict("a different owner superseded this request"),
        ApiError::from_status(StatusCode::SERVICE_UNAVAILABLE, "ordinary unavailable"),
    ] {
        let expected_status = error.status;
        let expected_message = error.message.clone();
        let response = error
            .at_durability_boundary(DurabilityPendingOperation::TestRunRegistration)
            .into_response();
        assert_eq!(response.status(), expected_status);
        let bytes = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"error": expected_message})
        );
    }
    // A shared helper's deadline is not, on its own, a new public contract.
    let response = ApiError::enqueued_durability_deadline(
        "shared authority helper",
        json!({"ownerId": "original-owner"}),
    )
    .into_response();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({"error": "shared authority helper"})
    );
}

#[tokio::test]
async fn durability_pending_wire_preserves_the_exact_existing_identity_for_each_boundary() {
    for (operation, name) in [
        (
            DurabilityPendingOperation::TestRunRegistration,
            "test_run_registration",
        ),
        (
            DurabilityPendingOperation::AcceptanceEvaluationSubmission,
            "acceptance_evaluation_submission",
        ),
        (
            DurabilityPendingOperation::NamedRootNaming,
            "named_root_naming",
        ),
        (
            DurabilityPendingOperation::NamedRootRelease,
            "named_root_release",
        ),
    ] {
        let identity = json!({"existing": "unchanged"});
        let response = ApiError::enqueued_durability_deadline(
            "the enqueued write has not been acknowledged",
            identity.clone(),
        )
        .at_durability_boundary(operation)
        .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["contract"], "termal-durability-pending");
        assert_eq!(body["version"], 1);
        assert_eq!(body["code"], "durability_pending");
        assert_eq!(body["outcome"], "pending");
        assert_eq!(body["operation"], name);
        assert_eq!(body["identity"], identity);
        assert_eq!(body["retry"], json!({"mode": "same_request"}));
        assert!(body["error"].as_str().unwrap().contains("may still commit"));
        assert!(body.get("notApplied").is_none());
    }
}

#[tokio::test]
async fn durability_pending_registration_replays_the_same_operation_after_a_late_commit() {
    let _temp_scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = FenceFixture::new();
    let mut state = test_app_state();
    let root = fixture.root.path().join("registration-project");
    fs::create_dir(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project = create_test_project(&state, &root, "Pending registration");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    let run_id = "test-durability-pending-registration";
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

    let (ready_tx, ready_rx) = mpsc::channel();
    let (reply_tx, reply_rx) = mpsc::channel();
    let worker = {
        let state = state.clone();
        let session = session.clone();
        let run_directory = test_run_display_path(&directory);
        std::thread::spawn(move || {
            TEST_TEST_RUN_REGISTRATION_FENCE_SENT.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || ready_tx.send(()).unwrap()));
            });
            reply_tx
                .send(state.register_test_run(
                    &session,
                    RegisterTestRunRequest {
                        run_id: run_id.to_owned(),
                        run_directory,
                    },
                ))
                .unwrap();
        })
    };
    ready_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    fixture.receive(false);
    let PersistRequest::Fence(fence) = fixture.rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap()
    else {
        panic!("the real registration fence follows its Delta");
    };
    let expected = match &fence.target {
        PersistFenceTarget::TestRunRegistration(expected) => expected.clone(),
        _ => panic!("registration content fence"),
    };
    assert!(matches!(
        reply_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(
        !state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .registered
            .contains(run_id)
    );

    // The same completion clock boundary that a real deadline polls. No
    // sleep, slow disk, injected successful ACK, or timeout inflation.
    assert!(matches!(
        fence.completion.poll_at(fence.completion.deadline),
        Some(Err(PersistFenceError::Deadline))
    ));
    let error = match reply_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap() {
        Err(error) => error,
        Ok(_) => panic!("an expired fence must not acknowledge registration"),
    };
    worker.join().unwrap();
    assert!(
        !state
            .inner
            .lock()
            .unwrap()
            .test_runs
            .registered
            .contains(run_id)
    );
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    let pending: Value = serde_json::from_slice(&bytes).unwrap();

    // The timed-out operation still commits. This is why a deadline cannot
    // say "not applied" or authorize a newly minted run identity.
    fixture.batch.accept(PersistRequest::Fence(fence));
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    // Reload deliberately marks a still-live registration interrupted. Assert
    // that recovery fact, then compare the complete durable identity exactly.
    let replay_expected = loaded.test_run_registrations[run_id].clone();
    let mut recovered = replay_expected.clone();
    assert!(
        recovered.monitoring_interrupted_at.take().is_some(),
        "boot recovery must mark the still-live registration interrupted"
    );
    assert_eq!(recovered, expected);

    // New host-local indexes and locks, populated only by the actual SQLite
    // loader, not a copy of the timed-out process's in-memory registration.
    let mut restarted = test_app_state();
    restarted.persistence_path = state.persistence_path.clone();
    restarted.inner = Arc::new(StateMutex::new(loaded));
    restarted.persist_tx = fixture.tx.clone();
    assert!(!Arc::ptr_eq(&restarted.inner, &state.inner));
    fixture.inner = restarted.inner.clone();
    let state = restarted;
    let original_request = fs::read(directory.join("request.json")).unwrap();
    let mut changed: Value = serde_json::from_slice(&original_request).unwrap();
    changed["stages"] = json!([{"name": "different-stage"}]);
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    let error = match state.register_test_run(
        &session,
        RegisterTestRunRequest {
            run_id: run_id.to_owned(),
            run_directory: test_run_display_path(&directory),
        },
    ) {
        Err(error) => error,
        Ok(_) => panic!("a changed immutable request cannot replay a loaded registration"),
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.durability_deadline.is_none());
    assert_eq!(
        state.inner.lock().unwrap().test_run_registrations[run_id],
        replay_expected
    );
    fs::write(directory.join("request.json"), original_request).unwrap();

    let (ready_tx, ready_rx) = mpsc::channel();
    let (reply_tx, reply_rx) = mpsc::channel();
    let worker = {
        let state = state.clone();
        let session = session.clone();
        let run_directory = test_run_display_path(&directory);
        std::thread::spawn(move || {
            TEST_TEST_RUN_REGISTRATION_FENCE_SENT.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || ready_tx.send(()).unwrap()));
            });
            reply_tx
                .send(state.register_test_run(
                    &session,
                    RegisterTestRunRequest {
                        run_id: run_id.to_owned(),
                        run_directory,
                    },
                ))
                .unwrap();
        })
    };
    ready_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
    fixture.receive(false);
    assert!(!fixture.batch.drain(&fixture.rx, false));
    let delta = fixture.collect();
    fixture.write(&delta).unwrap();
    let replay = match reply_rx.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap() {
        Ok(replay) => replay,
        Err(error) => panic!("same-identity replay failed: {}", error.message),
    };
    worker.join().unwrap();
    assert!(replay.registered);
    assert_eq!(replay.run_id, run_id);
    let inner = state.inner.lock().unwrap();
    assert_eq!(inner.test_run_registrations.len(), 1);
    assert_eq!(
        inner.test_run_registrations.get(run_id),
        Some(&replay_expected)
    );
    drop(inner);

    // This contract assertion is deliberately last, after all fixture workers
    // are joined and the actual late commit and unchanged replay are checked.
    // On the old implementation it fails with HTTP 500, not a compile error.
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{pending}");
    assert_eq!(pending["contract"], "termal-durability-pending");
    assert_eq!(pending["version"], 1);
    assert_eq!(pending["code"], "durability_pending");
    assert_eq!(pending["outcome"], "pending");
    assert_eq!(pending["operation"], "test_run_registration");
    assert_eq!(pending["identity"]["runId"], run_id);
    assert_eq!(
        pending["identity"]["requestDigest"],
        expected.request_identity
    );
    assert_eq!(pending["retry"]["mode"], "same_request");
}
