//! Remote observation diagnostics cross the real delivery, retry settlement,
//! transcript and persistence boundaries. Fixtures never invoke an owner binary.
use super::*;

fn owner_report(fixture: &ObservationSession) -> EngramRuntimeObservationReport {
    let mut capture = observation_capture();
    let EngramRuntimeClosingSource::Measured(sighting) = &mut capture.source else { panic!() };
    sighting.source_basis.source_revision.clear();
    sighting.source_basis.source_id = Some(serde_json::from_value(json!({
        "commit": "a".repeat(40), "tree": "b".repeat(40), "submodules": [],
        "changes": [{"path": "private-owner-path.rs", "status": "??", "origin": null,
            "blob": "c".repeat(40), "mode": "100644"}]
    })).unwrap());
    let root = FsPath::new(&fixture.state.default_workdir);
    EngramRuntimeObservationReport::prepare(&observation_trace(), capture,
        observation_connection(root), observation_store(root), "fixture-observer".into(),
        Duration::from_secs(1)).unwrap()
}

fn notices(fixture: &ObservationSession) -> String {
    fixture.record(|record| record.session.messages.iter().filter_map(|message| match message {
        Message::Text { author: Author::System, text, .. } => Some(text.as_str()),
        _ => None,
    }).collect::<Vec<_>>().join("\n"))
}

#[test]
fn ordinary_observation_remote_echo_is_retained_without_entering_transcript_or_delta() {
    for (code, policy_failure, expected_notice) in [
        (ENGRAM_IDEMPOTENCY_CONFLICT_CODE, false, "identity collision"),
        ("invalid_request", false, "Engram refused"),
        ("PRIVATE_REMOTE_CODE", false, "unresolved"),
        ("", true, "runtime_observation_policy_unavailable"),
    ] {
        let fixture = ObservationSession::new();
        let mut report = owner_report(&fixture);
        report.attempts = ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS - 1;
        fixture.retain(report.clone());
        fixture.persist();
        let payload = format!("DISTINCT_REMOTE_ECHO {}", report.envelopes[0].request);
        assert!(payload.contains("source_id"), "the error really echoes an owner-bearing request");
        let transport = Arc::new(ObservationReplies::default());
        if policy_failure {
            *transport.policy_error.lock().unwrap() = Some(EngramTransportError::protocol(&payload));
        } else {
            transport.replies.lock().unwrap().push_back(Err(EngramTransportError::remote(
                EngramControlErrorBody { code: code.into(), message: payload.clone() })));
        }
        let adapter = observation_adapter(transport.clone(), FsPath::new(&fixture.state.default_workdir));
        let mut next = deliver_runtime_observation_envelope(&adapter, &report, 0, Duration::from_secs(1)).unwrap();
        finish_runtime_observation_delivery_attempt(&mut next, &fixture.id, 0);
        let retained = match &next.envelopes[0].delivery {
            EngramRuntimeObservationDelivery::Refused { message, .. }
            | EngramRuntimeObservationDelivery::LocalFailure { message, .. } => message,
            other => panic!("delivery must settle at this boundary: {other:?}"),
        };
        assert!(retained.contains(&payload), "internal evidence keeps the complete error");
        assert_eq!(next.envelopes[0].request, report.envelopes[0].request);
        let mut deltas = fixture.state.subscribe_delta_events();
        fixture.state.install_runtime_observation_result(&fixture.id, &report, next.clone(), 0).unwrap();
        let transcript = notices(&fixture);
        let delta = deltas.try_recv().expect("settlement publishes its transcript diagnostic");
        assert!(transcript.contains(expected_notice));
        for automatic in [&transcript, &delta] {
            for private in ["DISTINCT_REMOTE_ECHO", "PRIVATE_REMOTE_CODE", "private-owner-path.rs", "source_id"] {
                assert!(!automatic.contains(private), "remote data escaped through {code}");
            }
        }
        assert!(runtime_observation_report_is_durable(&fixture.state.persistence_path, &fixture.id, &next).unwrap());
        assert_eq!(transport.sent.lock().unwrap().len(), usize::from(!policy_failure));
    }
}

#[test]
fn ordinary_observation_closing_failure_keeps_remote_details_out_of_transcript() {
    let fixture = ObservationSession::new();
    let root = FsPath::new(&fixture.state.default_workdir);
    let mut job = EngramRuntimeClosingJob::new(observation_trace(), None, EngramRuntimeClosingScope {
        binding: Some(observation_binding()), store: observation_store(root),
        connection: observation_connection(root), historical_fields: BTreeMap::new(),
        root: EngramNamedRootState::None, workdir: fixture.state.default_workdir.clone(),
    });
    job.attempts = ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS - 1;
    fixture.record(|record| record.engram.unadmitted_closing_jobs.push(job.clone()));
    let payload = format!("DISTINCT_CLOSING_ECHO {}", owner_report(&fixture).envelopes[0].request);
    fixture.state.install_runtime_closing_result(&fixture.id, &job,
        Err(EngramTransportError::remote(EngramControlErrorBody {
            code: "PRIVATE_CLOSING_CODE".into(), message: payload.clone(),
        }))).unwrap();
    let transcript = notices(&fixture);
    assert!(transcript.contains("runtime_observation_failed"));
    assert!(!transcript.contains("DISTINCT_CLOSING_ECHO"));
    assert!(!transcript.contains("PRIVATE_CLOSING_CODE"));
    assert!(!transcript.contains("source_id"));
    fixture.record(|record| {
        let failed = &record.engram.unadmitted_closing_jobs[0];
        assert_eq!(failed.terminal_failure.as_deref(), Some(payload.as_str()));
        assert_eq!(failed.last_error.as_deref(), Some(payload.as_str()));
    });
}
