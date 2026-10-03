//! Deletion capture/persistence and remote-failure regressions on public paths.
use super::*;
use crate::tests::*;

struct CaptureDeleteWriter {
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CaptureDeleteWriter {
    fn finish(mut self) {
        self.done.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

impl Drop for CaptureDeleteWriter {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

#[test]
fn source_observation_delete_capture_waits_for_explicit_coupled_recovery() {
    for quarantine in [false, true] {
        capture_delete_recovery_case(quarantine, false);
    }
}

#[test]
fn source_observation_delete_superseded_capture_waits_for_explicit_coupled_recovery() {
    for quarantine in [false, true] {
        capture_delete_recovery_case(quarantine, true);
    }
}

fn capture_delete_recovery_case(quarantine: bool, superseded: bool) {
    let mut claimed = opening_fixture("source-observation-delete-capture-ack");
    let (tx, rx) = std::sync::mpsc::channel();
    claimed.state.persist_tx = tx;
    let state = claimed.state.clone();
    let id = claimed.session_id.clone();
    let ready = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let writer_ready = ready.clone();
    let writer_done = done.clone();
    let writer = std::thread::spawn(move || {
        let mut cache = SqlitePersistConnectionCache::new();
        let prime = collect_persist_delta_from_shared_state(&state.inner, 0);
        persist_delta_via_cache(&mut cache, state.persistence_path.as_path(), &prime).unwrap();
        let mut watermark = prime.watermark;
        let mut batch = PersistFenceBatch::default();
        let mut failed_marker = false;
        while !writer_done.load(Ordering::SeqCst) {
            if let Ok(PersistRequest::Fence(fence)) = rx.recv_timeout(Duration::from_millis(20)) {
                let mut delta = collect_persist_delta_from_shared_state(&state.inner, watermark);
                let recovering = writer_ready.load(Ordering::SeqCst);
                let removal_fence = matches!(
                    &fence.target,
                    PersistFenceTarget::EngramSourceRemoval { .. }
                );
                // Admission must commit normally. Withhold the row only once
                // Delete reaches its coupled marker fence, then keep it withheld
                // through automatic capture completion until explicit recovery.
                if !recovering && quarantine && (failed_marker || removal_fence) {
                    delta
                        .changed_sessions
                        .retain(|record| record.session.id != id);
                    delta.deferred_session_ids.push(id.clone());
                }
                if !recovering && removal_fence {
                    failed_marker = true;
                    persist_delta_via_cache(&mut cache, state.persistence_path.as_path(), &delta)
                        .unwrap();
                    watermark = delta.watermark;
                    assert_eq!(
                        fence
                            .target
                            .is_already_durable(cache.connection.as_ref().unwrap())
                            .unwrap(),
                        !quarantine
                    );
                    fence.finish(Err(PersistFenceError::WriteFailed(
                        "marker ACK unavailable during capture".to_owned(),
                    )));
                } else {
                    batch.accept(PersistRequest::Fence(fence));
                    persist_delta_with_fences(
                        &mut cache,
                        state.persistence_path.as_path(),
                        &delta,
                        &mut batch,
                    )
                    .unwrap();
                    watermark = delta.watermark;
                }
            }
        }
        batch.fail(PersistFenceError::Shutdown);
        assert!(failed_marker);
    });
    let writer = CaptureDeleteWriter {
        done,
        thread: Some(writer),
    };
    let state = claimed.state.clone();
    let id = claimed.session_id.clone();
    TEST_ENGRAM_DURING_TURN_START_CAPTURE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let error = state
                .kill_session(&id)
                .err()
                .expect("capture Delete must retain its owner");
            assert!(error.message.contains("persistence is uncertain"));
        }));
    });
    let dispatch = claimed.dispatch();
    if superseded {
        let state = claimed.state.clone();
        let id = claimed.session_id.clone();
        let prompt_id = claimed.record(|record| {
            record
                .queued_prompts
                .front()
                .unwrap()
                .pending_prompt
                .id
                .clone()
        });
        TEST_ENGRAM_BEFORE_SOURCE_OBSERVATION_CAPTURE.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                state.cancel_queued_prompt(&id, &prompt_id).unwrap();
            }));
        });
    }
    let outcome = deliver_turn_dispatch(&claimed.state, dispatch);
    let requests = claimed.transport.requests();
    let premature_closes = requests
        .iter()
        .filter(|r| r.request["operation"] == "turn_checkpoint")
        .count();
    // Always release and join the real writer before a failing assertion.
    ready.store(true, Ordering::SeqCst);
    let recovery = claimed
        .state
        .resume_local_session_queue(&claimed.session_id, None);
    writer.finish();
    if superseded {
        assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Superseded));
    } else {
        assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Held { .. }));
    }
    assert_eq!(
        premature_closes, 0,
        "a failed deletion marker ACK must park capture settlement until explicit Resume"
    );
    recovery.unwrap();
    assert_eq!(
        claimed
            .transport
            .requests()
            .iter()
            .filter(|r| r.request["operation"] == "turn_checkpoint")
            .count(),
        1
    );
    assert!(claimed.runtime_rx.try_recv().is_err());
}

#[test]
fn source_observation_delete_remote_refusal_preserves_proxy_queue_authority() {
    let state = test_app_state();
    let remote = crate::tests::project_creation_races::remote_config("remote-delete-refusal");
    let project = create_test_remote_project(
        &state,
        &remote,
        "/remote/delete",
        "Remote Delete",
        "remote-project-1",
    );
    let remote_state = sample_remote_orchestrator_state(
        "remote-project-1",
        "/remote/delete",
        3,
        OrchestratorInstanceStatus::Running,
    );
    let mut session = remote_state.sessions[0].clone();
    session.pending_prompts = serde_json::from_value(json!([{
        "id": "remote-pending", "text": "Remote queued work", "timestamp": "12:00"
    }]))
    .unwrap();
    let local = {
        let mut inner = state.inner.lock().unwrap();
        let id =
            upsert_remote_proxy_session_record(&mut inner, &remote.id, &session, Some(project));
        state.commit_locked(&mut inner).unwrap();
        id
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let reply = serde_json::to_string(&remote_state).unwrap();
    let server = std::thread::spawn(move || {
        loop {
            let mut stream = accept_test_connection(&listener, "remote delete refusal listener");
            let request = read_test_http_request(&mut stream);
            if request.request_line.starts_with("GET /api/health ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::OK,
                    "application/json",
                    r#"{"ok":true,"serverInstanceId":"remote-test-instance"}"#,
                );
            } else if request.request_line.contains("/kill ") {
                write_test_http_response(
                    &mut stream,
                    StatusCode::CONFLICT,
                    "application/json",
                    r#"{"error":"remote session retained"}"#,
                );
            } else {
                assert!(
                    request.request_line.contains("/queue/resume "),
                    "{}",
                    request.request_line
                );
                write_test_http_response(&mut stream, StatusCode::OK, "application/json", &reply);
                break;
            }
        }
    });
    insert_test_remote_connection(
        &state,
        &remote,
        port,
        TestRemoteBridgeOwnership::RequestOnly,
    );
    assert!(state.kill_session(&local).is_err());
    // A metadata refresh has no queue-bearing message delta to clear a latch.
    state
        .apply_remote_delta_event(
            &remote.id,
            DeltaEvent::SessionCreated {
                revision: 2,
                session_id: session.id.clone(),
                session: test_state_session_summary_from_session(&session),
            },
        )
        .unwrap();
    let resumed = state.resume_session_queue(&local);
    join_test_server(server);
    resumed.unwrap();
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&local).unwrap()];
    assert!(
        !record.engram.source_observation_delete_requested,
        "failed ordinary remote Delete must not install local recovery authority"
    );
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert!(!AppState::wire_session_from_record(&state.server_instance_id, record).queue_paused);
    assert!(
        !AppState::wire_session_summary_from_record(&state.server_instance_id, record).queue_paused
    );
}
