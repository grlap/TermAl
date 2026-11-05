//! Delayed ordinary capture/delivery and exact deletion acknowledgement races.
use super::*;
use crate::tests::*;

#[test]
fn ordinary_observation_public_delete_retains_on_lost_or_deferred_removal_ack() {
    for defer_row in [false, true] {
        let mut fixture = ObservationSession::new();
        let mut report = fixture.report();
        for envelope in &mut report.envelopes {
            envelope.delivery = EngramRuntimeObservationDelivery::LocalFailure {
                code: "runtime_observation_retry_exhausted".into(), message: "unknown outcome".into(),
            };
        }
        fixture.retain(report.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        fixture.state.persist_tx = tx;
        let state = fixture.state.clone();
        let id = fixture.id.clone();
        let deleting = std::thread::spawn(move || state.kill_session(&id));
        let fence = loop {
            match rx.recv_timeout(DEADLOCK_GUARD).expect("public Delete must issue its removal fence") {
                PersistRequest::Fence(fence) => break fence,
                _ => {},
            }
        };
        assert!(matches!(&fence.target, PersistFenceTarget::EngramObservationRemoval { .. }));
        let mut delta = collect_persist_delta_from_shared_state(&fixture.state.inner, 0);
        assert!(delta.changed_sessions.iter().any(|record| record.session.id == fixture.id));
        if defer_row {
            delta.changed_sessions.retain(|record| record.session.id != fixture.id);
            delta.deferred_session_ids.push(fixture.id.clone());
        }
        let mut cache = SqlitePersistConnectionCache::new();
        persist_delta_via_cache(&mut cache, fixture.state.persistence_path.as_ref(), &delta).unwrap();
        assert_eq!(fence.target.is_already_durable(cache.connection.as_ref().unwrap()).unwrap(), !defer_row);
        fence.finish(Err(PersistFenceError::WriteFailed("removal acknowledgement unavailable".into())));
        let error = deleting.join().unwrap().err().expect("public Delete cannot use an unknown local acknowledgement");
        assert!(error.message.contains("persistence is uncertain"));
        fixture.record(|record| {
            assert!(record.engram.observation_delete_requested && record.orchestrator_auto_dispatch_blocked);
            assert_eq!(record.engram.unadmitted_reports, vec![report]);
        });
        assert!(fixture.state.inner.lock().unwrap().find_visible_session_index(&fixture.id).is_some());
    }
}

fn observation_closing_job(fixture: &ObservationSession) -> EngramRuntimeClosingJob {
    let root = FsPath::new(&fixture.state.default_workdir);
    EngramRuntimeClosingJob::new(observation_trace(), None, EngramRuntimeClosingScope {
        binding: Some(observation_binding()), store: observation_store(root),
        connection: observation_connection(root), historical_fields: BTreeMap::new(),
        root: EngramNamedRootState::None, workdir: fixture.state.default_workdir.clone(),
    })
}

#[test]
fn ordinary_observation_superseded_capture_cannot_overwrite_a_newer_closing_job() {
    let fixture = ObservationSession::new();
    let old = observation_closing_job(&fixture);
    let mut newer = old.clone();
    newer.attempts += 1;
    newer.policy = Some(observation_policy());
    fixture.record(|record| record.engram.unadmitted_closing_jobs.push(newer.clone()));
    fixture.state.install_runtime_closing_result(&fixture.id, &old,
        Ok(EngramRuntimeClosingOutcome::Report(fixture.report(), None))).unwrap();
    fixture.record(|record| {
        assert_eq!(record.engram.unadmitted_closing_jobs, vec![newer]);
        assert!(record.engram.unadmitted_reports.is_empty());
        assert!(record.engram.runtime_checkpoint.is_none());
    });
}

#[test]
fn ordinary_observation_delete_during_capture_transfers_custody_to_original_report() {
    for source_free in [false, true] {
        let fixture = ObservationSession::new();
        let job = observation_closing_job(&fixture);
        let mut capture = observation_capture();
        if source_free {
            capture.source = EngramRuntimeClosingSource::UnsupportedGitlinks {
                observed_at: observation_at(50),
            };
        }
        let root = FsPath::new(&fixture.state.default_workdir);
        let report = EngramRuntimeObservationReport::prepare(&job.trace, capture,
            observation_connection(root), observation_store(root),
            "fixture-observer".into(), Duration::from_secs(1)).unwrap();
        if source_free {
            assert_eq!(report.envelopes.len(), 1, "the source-free report has no sighting");
        }
        let mut successor = observation_trace();
        successor.host_turn_ref = "newer-active-turn".into();
        successor.generation += 1;
        fixture.record(|record| {
            record.engram.unadmitted_closing_jobs.push(job.clone());
            record.engram.unadmitted_active_turn = Some(successor.clone());
            record.active_turn_generation = successor.generation;
            record.engram.observation_delete_requested = true;
            record.set_auto_dispatch_blocked(true);
        });
        let refusal = source_free.then_some(SourceBasisRefusal::UnsupportedGitlinks);
        fixture.state.install_runtime_closing_result(&fixture.id, &job,
            Ok(EngramRuntimeClosingOutcome::Report(report.clone(), refusal))).unwrap();
        fixture.record(|record| {
            assert!(record.engram.unadmitted_closing_jobs.is_empty());
            assert_eq!(record.engram.unadmitted_reports, vec![report]);
            assert_eq!(record.engram.unadmitted_active_turn, Some(successor));
            assert!(record.engram.observation_delete_requested && record.orchestrator_auto_dispatch_blocked);
            assert!(runtime_observation_has_removal_duty(record));
            let notices = record.session.messages.iter().filter(|message| matches!(message,
                Message::Text { author: Author::System, text, .. }
                    if text.contains("has no measured source"))).count();
            assert_eq!(notices, usize::from(source_free));
        });
    }
}

#[test]
fn ordinary_observation_late_receipt_preserves_delete_marker_and_successor() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    fixture.retain(report.clone());
    fixture.record(|record| {
        record.engram.observation_delete_requested = true;
        record.set_auto_dispatch_blocked(true);
        record.session.preview = "Delete retained this session".into();
        record.engram.unadmitted_active_turn = Some(observation_trace());
    });
    fixture.state.install_runtime_observation_result(&fixture.id, &report, recorded_report(&report), 0).unwrap();
    fixture.record(|record| {
        assert!(record.engram.observation_delete_requested && record.orchestrator_auto_dispatch_blocked);
        assert!(record.engram.unadmitted_active_turn.is_some());
        assert_eq!(record.session.preview, "Delete retained this session");
        assert!(record.engram.runtime_checkpoint.is_none());
    });
}

#[test]
fn ordinary_observation_closing_failure_retains_original_scope_without_inventing_a_report() {
    let fixture = ObservationSession::new();
    let mut job = observation_closing_job(&fixture);
    job.attempts = ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS - 1;
    fixture.record(|record| record.engram.unadmitted_closing_jobs.push(job.clone()));
    fixture.state.install_runtime_closing_result(&fixture.id, &job,
        Err(EngramTransportError::transport("capture unavailable"))).unwrap();
    fixture.record(|record| {
        let failed = &record.engram.unadmitted_closing_jobs[0];
        assert_eq!(failed.scope, job.scope);
        assert_eq!(failed.trace, job.trace);
        assert!(failed.terminal_failure.is_some());
        assert!(record.engram.unadmitted_reports.is_empty());
        assert!(record.engram.runtime_checkpoint.is_none());
    });
}

#[test]
fn ordinary_observation_delayed_result_after_connection_replacement_uses_retained_scope() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    fixture.retain(report.clone());
    fixture.record(|record| {
        record.engram.work_binding = Some(EngramControlWorkBinding { run_id: "successor-run".into(), ..observation_binding() });
    });
    let transport = Arc::new(ObservationReplies::default());
    transport.replies.lock().unwrap().push_back(Ok(observation_receipt(&report_observation(&report, 0), &report.observing_session)));
    let adapter = observation_adapter(transport.clone(), FsPath::new(&fixture.state.default_workdir));
    let next = deliver_runtime_observation_envelope(&adapter, &report, 0, Duration::from_secs(1)).unwrap();
    fixture.state.install_runtime_observation_result(&fixture.id, &report, next, 0).unwrap();
    let sent = transport.sent.lock().unwrap();
    assert_eq!(sent[0].0, report.connection);
    assert_eq!(sent[0].1, report.envelopes[0].request);
    fixture.record(|record| assert_eq!(record.engram.work_binding.as_ref().unwrap().run_id, "successor-run"));
}

#[test]
fn ordinary_observation_delete_ack_requires_commit_receipt_and_current_owner() {
    for race in ["lost_ack", "owner_changed", "success"] {
        let mut fixture = ObservationSession::new();
        fixture.retain(fixture.report());
        fixture.record(|record| {
            record.engram.observation_delete_requested = true;
            record.set_auto_dispatch_blocked(true);
        });
        let (tx, rx) = std::sync::mpsc::channel();
        fixture.state.persist_tx = tx;
        let state = fixture.state.clone();
        let id = fixture.id.clone();
        let task = std::thread::spawn(move || state.confirm_source_observation_removal_durable(&id));
        let fence = loop {
            match rx.recv_timeout(DEADLOCK_GUARD).expect("removal must issue a bounded persistence fence") {
                PersistRequest::Fence(fence) => break fence,
                _ => {},
            }
        };
        assert!(matches!(&fence.target, PersistFenceTarget::EngramObservationRemoval { .. }));
        let delta = collect_persist_delta_from_shared_state(&fixture.state.inner, 0);
        let mut cache = SqlitePersistConnectionCache::new();
        persist_delta_via_cache(&mut cache, fixture.state.persistence_path.as_ref(), &delta).unwrap();
        assert!(fence.target.is_already_durable(cache.connection.as_ref().unwrap()).unwrap());
        if race == "lost_ack" {
            fence.finish(Err(PersistFenceError::WriteFailed("commit acknowledgement lost".into())));
        } else if race == "owner_changed" {
            fixture.record(|record| record.engram.unadmitted_reports[0].version += 1);
            fence.finish(Ok(()));
        } else {
            fence.finish(Ok(()));
        }
        assert_eq!(task.join().unwrap().is_ok(), race == "success", "wrong disposition for {race}");
        fixture.record(|record| {
            assert!(record.engram.observation_delete_requested);
            assert!(record.orchestrator_auto_dispatch_blocked);
            assert!(runtime_observation_has_removal_duty(record));
        });
    }
}

#[test]
fn ordinary_observation_retired_process_cannot_install_delayed_capture_or_receipt() {
    let fixture = ObservationSession::new();
    let report = fixture.report();
    let job = observation_closing_job(&fixture);
    fixture.retain(report.clone());
    fixture.record(|record| record.engram.unadmitted_closing_jobs.push(job.clone()));
    fixture.state.runtime_observation_lifetime.stop_and_join();
    fixture.state.install_runtime_observation_result(&fixture.id, &report, recorded_report(&report), 0).unwrap();
    fixture.state.install_runtime_closing_result(&fixture.id, &job,
        Ok(EngramRuntimeClosingOutcome::Report(report.clone(), None))).unwrap();
    fixture.record(|record| {
        assert_eq!(record.engram.unadmitted_reports, vec![report]);
        assert_eq!(record.engram.unadmitted_closing_jobs, vec![job]);
        assert!(record.engram.runtime_checkpoint.is_none());
    });
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
        !record.engram.observation_delete_requested,
        "failed ordinary remote Delete must not install local recovery authority"
    );
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert!(!AppState::wire_session_from_record(&state.server_instance_id, record).queue_paused);
    assert!(
        !AppState::wire_session_summary_from_record(&state.server_instance_id, record).queue_paused
    );
}
