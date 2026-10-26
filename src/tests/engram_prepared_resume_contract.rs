//! Explicit Resume after a replay's local acknowledgement failure.
//! Mounted by engram_begin_unknown_recovery.rs for its stateful producer/writer.
use super::*;

struct UnbegunResumeProducer {
    producer: Arc<StatefulEngramControlTransport>,
    unavailable: AtomicBool,
    begins: Mutex<Vec<Value>>,
    log: Arc<BoundaryLog>,
}

impl EngramControlTransport for UnbegunResumeProducer {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        if matches!(request, EngramControlRequest::TurnBegin { .. }) {
            let wire = serde_json::to_value(request).unwrap();
            self.begins.lock().unwrap().push(wire.clone());
            self.log.push(Boundary::Sent(wire));
            if self.unavailable.swap(false, Ordering::SeqCst) {
                // The host cannot know this invocation never reached Engram.
                // Only the later producer refusal establishes that fact.
                return Err(EngramTransportError::transport("Begin reply unavailable"));
            }
        }
        self.producer.request(connection, request, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.producer.shutdown_session(session_id);
    }
}

#[test]
fn admission_contract_resume_expired_never_begun_grant_requires_producer_refusal() {
    let (mut state, session, receiver, _) = root_fixture([]);
    let transport = Arc::new(UnbegunResumeProducer {
        producer: StatefulEngramControlTransport::with_first_begin_refusal("grant_expired"),
        unavailable: AtomicBool::new(true),
        begins: Mutex::new(Vec::new()),
        log: Arc::default(),
    });
    state.install_control_test_transport(transport.clone());
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailSequence(vec![
            FenceStep::Acknowledge,
            FenceStep::Fail(PersistFenceError::Deadline),
        ])),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    tick_past_any_due(&state, &session);
    let before = with_record(&state, &session, |record| {
        (
            record.queued_prompts.front().unwrap().engram_interrupted,
            record.engram.uncertain_grant_id.clone(),
        )
    });
    let calls_before = transport.begins.lock().unwrap().clone();
    let issued_before = transport.producer.grant_state(&session);
    let result = state.resume_session_queue(&session);
    for _ in 0..3 {
        if !with_record(&state, &session, |record| {
            record.engram.admission_retry.is_some()
        }) {
            break;
        }
        tick_past_any_due(&state, &session);
    }
    let calls = transport.begins.lock().unwrap().clone();
    let requests = transport.producer.requests();
    let prompts = abort_retry::prompts_received(&receiver);
    let active = with_record(&state, &session, |record| {
        record.engram.active_grant_id.clone()
    });
    writer.finish(&state);
    assert!(before.0);
    assert_eq!(calls_before.len(), 1, "the local hold sent no second Begin");
    assert!(
        issued_before.1.is_none(),
        "the producer had not begun the first grant"
    );
    assert_eq!(issued_before.0, before.1);
    assert!(result.is_ok());
    assert_eq!(
        calls.len(),
        3,
        "original, exact reconciliation, then fresh authorized Begin"
    );
    assert_eq!(calls[0], calls[1], "expiry is not inferred locally");
    assert_ne!(calls[1]["grant_id"], calls[2]["grant_id"]);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.request["operation"] == "turn_evaluate")
            .count(),
        2
    );
    assert_eq!(active.as_deref(), calls[2]["grant_id"].as_str());
    assert_eq!(prompts, 1);
}

#[test]
fn admission_contract_resume_rejects_other_grants_authorities_and_live_attempts() {
    for fault in [
        "mismatch",
        "authority",
        "active",
        "stopping",
        "concurrent",
        "begun",
    ] {
        let (mut state, session, receiver, transport) = lost_begin_fixture(true);
        let writer = AcknowledgingWriter::install_with(
            &mut state,
            transport.log.clone(),
            Some(PreparedFence::FailSequence(vec![
                FenceStep::Acknowledge,
                FenceStep::Fail(PersistFenceError::Deadline),
            ])),
        );
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        tick_past_any_due(&state, &session);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            match fault {
                "mismatch" => record.engram.uncertain_grant_id = Some("another-grant".into()),
                "authority" => {
                    record
                        .queued_prompts
                        .front_mut()
                        .unwrap()
                        .engram_evaluate
                        .as_mut()
                        .unwrap()
                        .connection
                        .actor_id = "another-actor".into()
                }
                "active" => record.engram.active_grant_id = Some("another-live-grant".into()),
                "stopping" => record.runtime_stop_in_progress = true,
                "concurrent" => record.session.status = SessionStatus::Active,
                "begun" => {
                    record
                        .queued_prompts
                        .front_mut()
                        .unwrap()
                        .engram_evaluate
                        .as_mut()
                        .unwrap()
                        .begun_grant_id = Some("already-handed-off".into())
                }
                _ => unreachable!(),
            }
        }
        let before = with_record(&state, &session, |record| {
            record
                .queued_prompts
                .front()
                .unwrap()
                .engram_evaluate
                .clone()
                .unwrap()
                .prepared_begin
        });
        let result = state.resume_session_queue(&session);
        let after = with_record(&state, &session, |record| {
            record
                .queued_prompts
                .front()
                .unwrap()
                .engram_evaluate
                .clone()
                .unwrap()
                .prepared_begin
        });
        let begins = begin_requests(&transport);
        let prompts = abort_retry::prompts_received(&receiver);
        writer.finish(&state);
        assert!(result.is_err(), "{fault} must retain its explicit hold");
        assert_eq!(before, after, "{fault} must preserve the exact request");
        assert_eq!(begins.len(), 1, "{fault} sent no replay");
        assert_eq!(prompts, 0, "{fault} sent no provider prompt");
    }
}

#[test]
fn admission_contract_resume_replays_uncertain_begin_after_local_deadline() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(true);
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailSequence(vec![
            FenceStep::Acknowledge,
            FenceStep::Fail(PersistFenceError::Deadline),
        ])),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    let original = begin_requests(&transport);
    tick_past_any_due(&state, &session);
    let before = with_record(&state, &session, |record| {
        let head = &record.queued_prompts[0];
        (
            head.engram_interrupted,
            head.engram_evaluate
                .as_ref()
                .unwrap()
                .prepared_begin
                .clone()
                .unwrap(),
            record.engram.uncertain_grant_id.clone(),
            record.engram.admission_retry.is_none()
                && record.engram.bind_retry.is_none()
                && record.engram.abort_retry.is_none(),
        )
    });
    let result = state.resume_session_queue(&session);
    let replayed = begin_requests(&transport);
    let prompts = abort_retry::prompts_received(&receiver);
    let after = with_record(&state, &session, |record| {
        (
            record.engram.active_grant_id.clone(),
            record.queued_prompts.len(),
        )
    });
    let injected = writer.injections();
    writer.finish(&state);

    assert_eq!(
        original.len(),
        1,
        "the producer applied one Begin before losing its reply"
    );
    assert_eq!(
        injected, 2,
        "the replay met the local acknowledgement deadline"
    );
    assert!(
        before.0 && before.3,
        "a local hold interrupted the otherwise exact replay"
    );
    assert_eq!(before.2.as_deref(), Some(before.1.grant_id.as_str()));
    assert!(
        result.is_ok(),
        "Resume must reconcile this exact uncertain Begin: {}",
        result.err().map(|e| e.message).unwrap_or_default()
    );
    assert_eq!(replayed.len(), 2, "Resume replays, it does not re-evaluate");
    assert_eq!(
        replayed[0], replayed[1],
        "all Begin fields are retained exactly"
    );
    assert_eq!(
        prompts, 1,
        "one provider handoff after the accepted replay receipt"
    );
    assert_eq!(after.0.as_deref(), Some(before.1.grant_id.as_str()));
    assert_eq!(after.1, 0);
    let evaluates = transport
        .producer
        .requests()
        .iter()
        .filter(|r| r.request["operation"] == "turn_evaluate")
        .count();
    assert_eq!(evaluates, 1, "the uncertain grant is never replaced");
}
