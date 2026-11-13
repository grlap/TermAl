//! Remote observation diagnostics cross the real delivery, retry settlement,
//! transcript and persistence boundaries. Fixtures never invoke an owner binary.
use super::*;

fn owner_report(fixture: &ObservationSession) -> EngramRuntimeObservationReport {
    let mut capture = observation_capture();
    let EngramRuntimeClosingSource::Measured(sighting) = &mut capture.source else {
        panic!()
    };
    sighting.source_basis.source_revision.clear();
    sighting.source_basis.source_id = Some(
        serde_json::from_value(json!({
            "commit": "a".repeat(40), "tree": "b".repeat(40), "submodules": [],
            "changes": [{"path": "private-owner-path.rs", "status": "??", "origin": null,
                "blob": "c".repeat(40), "mode": "100644"}]
        }))
        .unwrap(),
    );
    let root = FsPath::new(&fixture.state.default_workdir);
    EngramRuntimeObservationReport::prepare(
        &observation_trace(),
        capture,
        observation_connection(root),
        observation_store(root),
        "fixture-observer".into(),
        Duration::from_secs(1),
    )
    .unwrap()
}

fn notices(fixture: &ObservationSession) -> String {
    fixture.record(|record| {
        record
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::Text {
                    author: Author::System,
                    text,
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[test]
fn retained_revision_only_observations_retire_before_policy_or_send_and_keep_custody() {
    for revision in [
        "content-v1:private-old",
        "git-commit:private-old",
        "opaque-private-old",
    ] {
        for location in [
            "baseline",
            "comparison-sighting",
            "missing-baseline-sighting",
            "turn-check",
            "standalone-check",
            "sighting",
        ] {
            let fixture = ObservationSession::new();
            let mut trace = observation_trace();
            trace.commands.push(observation_command("check-one"));
            let mut capture = observation_capture();
            if location != "missing-baseline-sighting" {
                capture.baseline = Some(observation_endpoint());
            }
            let mut report = EngramRuntimeObservationReport::prepare(
                &trace,
                capture,
                observation_connection(FsPath::new(&fixture.state.default_workdir)),
                observation_store(FsPath::new(&fixture.state.default_workdir)),
                "fixture-observer".into(),
                Duration::from_secs(1),
            )
            .unwrap();
            let mut wire = report.envelopes[usize::from(location == "sighting")]
                .request
                .clone();
            if location == "standalone-check" {
                let check = wire["occurrence"]["observed_checks"][0].clone();
                wire["occurrence"] = json!({"kind":"observed_check", "host_turn_ref":trace.host_turn_ref, "check":check});
            }
            let path = match location {
                "baseline" => "/occurrence/source_change/baseline",
                "comparison-sighting" | "missing-baseline-sighting" => {
                    "/occurrence/source_change/sighting/source_basis"
                }
                "turn-check" => "/occurrence/observed_checks/0/source_basis",
                "standalone-check" => "/occurrence/check/source_basis",
                _ => "/occurrence/source_basis",
            };
            if matches!(location, "turn-check" | "standalone-check") {
                let check_path = if location == "turn-check" {
                    "/occurrence/observed_checks/0"
                } else {
                    "/occurrence/check"
                };
                wire.pointer_mut(check_path)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(
                        "source_basis".into(),
                        serde_json::to_value(observation_source("closing")).unwrap(),
                    );
            }
            let basis = wire.pointer_mut(path).unwrap().as_object_mut().unwrap();
            basis.remove("source_id");
            basis.insert("source_revision".into(), json!(revision));
            report.envelopes = vec![EngramRuntimeObservationEnvelope {
                request: wire.clone(),
                delivery: EngramRuntimeObservationDelivery::Pending {
                    last_error: Some("earlier reply lost after possible recording".into()),
                },
            }];
            // The saved predecessor image remains readable before retirement.
            let raw = serde_json::to_vec(&report).unwrap();
            let report: EngramRuntimeObservationReport = serde_json::from_slice(&raw).unwrap();
            assert_eq!(serde_json::to_vec(&report).unwrap(), raw);
            fixture.retain(report.clone());
            fixture.persist();
            let transport = Arc::new(ObservationReplies::default());
            *transport.policy_error.lock().unwrap() =
                Some(EngramTransportError::protocol("must not read policy"));
            let adapter = observation_adapter(
                transport.clone(),
                FsPath::new(&fixture.state.default_workdir),
            );
            let mut next =
                deliver_runtime_observation_envelope(&adapter, &report, 0, Duration::from_secs(1))
                    .unwrap();
            assert!(matches!(&next.envelopes[0].delivery,
                EngramRuntimeObservationDelivery::LocalFailure { code, message }
                if code == "legacy_source_input_retired"
                    && message.contains("earlier reply lost after possible recording")));
            assert_eq!(next.envelopes[0].request, wire);
            assert!(next.is_settled());
            assert!(
                !next.is_discharged(),
                "local retirement cannot certify an earlier remote outcome"
            );
            finish_runtime_observation_delivery_attempt(&mut next, &fixture.id, 0);
            assert_eq!(
                deliver_runtime_observation_envelope(&adapter, &next, 0, Duration::from_secs(1))
                    .unwrap(),
                next
            );
            assert_eq!(
                transport
                    .policy_calls
                    .load(std::sync::atomic::Ordering::SeqCst),
                0
            );
            assert!(transport.sent.lock().unwrap().is_empty());
            fixture
                .state
                .install_runtime_observation_result(&fixture.id, &report, next.clone(), 0)
                .unwrap();
            let diagnostic = notices(&fixture);
            assert!(diagnostic.contains("legacy_source_input_retired"));
            assert!(diagnostic.contains("retired locally"));
            assert!(diagnostic.contains("may be unknown"));
            assert!(!diagnostic.contains(revision));
            assert!(!diagnostic.contains("earlier reply lost"));
            assert!(
                runtime_observation_report_is_durable(
                    &fixture.state.persistence_path,
                    &fixture.id,
                    &next
                )
                .unwrap()
            );
            let restored = load_state(fixture.state.persistence_path.as_ref())
                .unwrap()
                .unwrap();
            let record = &restored.sessions[restored.find_session_index(&fixture.id).unwrap()];
            assert_eq!(record.engram.unadmitted_reports, vec![next]);
        }
    }
}

#[test]
fn source_id_observations_still_read_policy_and_deliver_with_legacy_words_in_evidence() {
    let fixture = ObservationSession::new();
    let mut trace = observation_trace();
    let mut command = observation_command("source-id-check");
    command.check.host_evidence_ref = Some("source_revision content-v1:historical-note".into());
    trace.commands.push(command);
    let mut report = EngramRuntimeObservationReport::prepare(
        &trace,
        observation_capture(),
        observation_connection(FsPath::new(&fixture.state.default_workdir)),
        observation_store(FsPath::new(&fixture.state.default_workdir)),
        "fixture-observer".into(),
        Duration::from_secs(1),
    )
    .unwrap();
    let transport = Arc::new(ObservationReplies::default());
    let originals = report.envelopes.clone();
    for index in 0..report.envelopes.len() {
        transport
            .replies
            .lock()
            .unwrap()
            .push_back(Ok(observation_receipt(
                &report_observation(&report, index),
                &report.observing_session,
            )));
    }
    let adapter = observation_adapter(
        transport.clone(),
        FsPath::new(&fixture.state.default_workdir),
    );
    for index in 0..report.envelopes.len() {
        report =
            deliver_runtime_observation_envelope(&adapter, &report, index, Duration::from_secs(1))
                .unwrap();
    }
    assert!(report.is_discharged());
    assert_eq!(
        transport
            .policy_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        originals.len()
    );
    let sent = transport.sent.lock().unwrap();
    assert_eq!(sent.len(), originals.len());
    for (index, envelope) in report.envelopes.iter().enumerate() {
        assert!(matches!(
            envelope.delivery,
            EngramRuntimeObservationDelivery::Recorded { .. }
        ));
        assert_eq!(envelope.request, originals[index].request);
        assert_eq!(sent[index].1, originals[index].request);
    }
}

#[test]
fn ordinary_observation_remote_echo_is_retained_without_entering_transcript_or_delta() {
    for (code, policy_failure, expected_notice) in [
        (
            ENGRAM_IDEMPOTENCY_CONFLICT_CODE,
            false,
            "identity collision",
        ),
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
        assert!(
            payload.contains("source_id"),
            "the error really echoes an owner-bearing request"
        );
        let transport = Arc::new(ObservationReplies::default());
        if policy_failure {
            *transport.policy_error.lock().unwrap() =
                Some(EngramTransportError::protocol(&payload));
        } else {
            transport
                .replies
                .lock()
                .unwrap()
                .push_back(Err(EngramTransportError::remote(EngramControlErrorBody {
                    code: code.into(),
                    message: payload.clone(),
                })));
        }
        let adapter = observation_adapter(
            transport.clone(),
            FsPath::new(&fixture.state.default_workdir),
        );
        let mut next =
            deliver_runtime_observation_envelope(&adapter, &report, 0, Duration::from_secs(1))
                .unwrap();
        finish_runtime_observation_delivery_attempt(&mut next, &fixture.id, 0);
        let retained = match &next.envelopes[0].delivery {
            EngramRuntimeObservationDelivery::Refused { message, .. }
            | EngramRuntimeObservationDelivery::LocalFailure { message, .. } => message,
            other => panic!("delivery must settle at this boundary: {other:?}"),
        };
        assert!(
            retained.contains(&payload),
            "internal evidence keeps the complete error"
        );
        assert_eq!(next.envelopes[0].request, report.envelopes[0].request);
        let mut deltas = fixture.state.subscribe_delta_events();
        fixture
            .state
            .install_runtime_observation_result(&fixture.id, &report, next.clone(), 0)
            .unwrap();
        let transcript = notices(&fixture);
        let delta = deltas
            .try_recv()
            .expect("settlement publishes its transcript diagnostic");
        assert!(transcript.contains(expected_notice));
        for automatic in [&transcript, &delta] {
            for private in [
                "DISTINCT_REMOTE_ECHO",
                "PRIVATE_REMOTE_CODE",
                "private-owner-path.rs",
                "source_id",
            ] {
                assert!(
                    !automatic.contains(private),
                    "remote data escaped through {code}"
                );
            }
        }
        assert!(
            runtime_observation_report_is_durable(
                &fixture.state.persistence_path,
                &fixture.id,
                &next
            )
            .unwrap()
        );
        assert_eq!(
            transport.sent.lock().unwrap().len(),
            usize::from(!policy_failure)
        );
    }
}

#[test]
fn ordinary_observation_closing_failure_keeps_remote_details_out_of_transcript() {
    let fixture = ObservationSession::new();
    let root = FsPath::new(&fixture.state.default_workdir);
    let mut job = EngramRuntimeClosingJob::new(
        observation_trace(),
        None,
        EngramRuntimeClosingScope {
            binding: Some(observation_binding()),
            store: observation_store(root),
            connection: observation_connection(root),
            historical_fields: BTreeMap::new(),
            root: EngramNamedRootState::None,
            workdir: fixture.state.default_workdir.clone(),
        },
    );
    job.attempts = ENGRAM_RUNTIME_OBSERVATION_MAX_ATTEMPTS - 1;
    fixture.record(|record| record.engram.unadmitted_closing_jobs.push(job.clone()));
    let payload = format!(
        "DISTINCT_CLOSING_ECHO {}",
        owner_report(&fixture).envelopes[0].request
    );
    fixture
        .state
        .install_runtime_closing_result(
            &fixture.id,
            &job,
            Err(EngramTransportError::remote(EngramControlErrorBody {
                code: "PRIVATE_CLOSING_CODE".into(),
                message: payload.clone(),
            })),
        )
        .unwrap();
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
