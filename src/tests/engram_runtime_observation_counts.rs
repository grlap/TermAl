use super::*;
use tower::ServiceExt;

fn envelope(
    occurrence: Value,
    delivery: EngramRuntimeObservationDelivery,
) -> EngramRuntimeObservationEnvelope {
    EngramRuntimeObservationEnvelope {
        request: json!({"operation":"execution_observe", "occurrence":occurrence,
            "idempotency_key":"private-request", "binding":{"private":"binding"}}),
        delivery,
    }
}

fn owner() -> Value {
    json!({"workspace_id":"private-workspace", "source_id":{
        "commit":"a".repeat(40), "tree":"b".repeat(40),
        "submodules":[{"path":"private-child", "commit":"c".repeat(40), "changes":[]}],
        "changes":[{"path":"private-path", "status":" M", "origin":null,
            "blob":"d".repeat(40), "mode":"100644"}]
    }})
}

fn legacy() -> Value {
    json!({"workspace_id":"private-workspace", "source_revision":"content-v1:private-revision"})
}
fn sighting(basis: Value) -> Value {
    json!({"kind":"source_sighting", "source_basis":basis})
}
fn pending() -> EngramRuntimeObservationDelivery {
    EngramRuntimeObservationDelivery::Pending {
        last_error: Some("private-error".into()),
    }
}
fn recorded() -> EngramRuntimeObservationDelivery {
    EngramRuntimeObservationDelivery::Recorded {
        receipt: json!({"private":"receipt"}),
    }
}
fn local_failure() -> EngramRuntimeObservationDelivery {
    EngramRuntimeObservationDelivery::LocalFailure {
        code: "private-code".into(),
        message: "private-message".into(),
    }
}

fn report(envelopes: Vec<EngramRuntimeObservationEnvelope>) -> EngramRuntimeObservationReport {
    EngramRuntimeObservationReport {
        host_turn_ref: "private-turn".into(),
        adopted_at: "private-time".into(),
        notice_message_id: "private-notice".into(),
        generation: 1,
        version: 1,
        observing_session: "private-session".into(),
        attempts: 8,
        retry_at: "private-retry".into(),
        call_timeout_ms: 1,
        connection: EngramConnectionConfig {
            binary_path: "unavailable-binary".into(),
            project_file: "unavailable-project".into(),
            home: "unavailable-home".into(),
            project_root: "unavailable-root".into(),
            actor_id: "private-actor".into(),
            actor_context: Some("private-context".into()),
            session_id: "private-session".into(),
        },
        store: EngramAuthorityStoreKey {
            database_path: "unavailable-database".into(),
            project_id: "private-project".into(),
        },
        envelopes,
    }
}

fn mixed_reports() -> Vec<EngramRuntimeObservationReport> {
    let mixed = json!({"kind":"inter_turn_change","source_change":{
        "detection":"content_comparison", "baseline":legacy(), "sighting":{"source_basis":owner()}
    }});
    vec![
        report(vec![
            envelope(sighting(legacy()), pending()),
            envelope(sighting(legacy()), recorded()),
        ]),
        report(vec![
            envelope(
                sighting(owner()),
                EngramRuntimeObservationDelivery::Refused {
                    code: "private-refusal".into(),
                    message: "private-refusal-body".into(),
                },
            ),
            envelope(sighting(owner()), local_failure()),
        ]),
        report(vec![envelope(
            json!({"kind":"unadmitted_turn","source_change":null,"observed_checks":[]}),
            pending(),
        )]),
        report(vec![
            envelope(sighting(legacy()), pending()),
            envelope(sighting(owner()), pending()),
        ]),
        report(vec![envelope(mixed, local_failure())]),
        report(vec![envelope(
            json!({"kind":"future-private-kind"}),
            recorded(),
        )]),
        report(vec![]),
    ]
}

#[test]
fn census_counts_historical_formats_as_unknown_without_rewriting_reports() {
    let reports = mixed_reports();
    let bytes = serde_json::to_vec(&reports).unwrap();
    let reloaded: Vec<EngramRuntimeObservationReport> = serde_json::from_slice(&bytes).unwrap();
    let counts = runtime_observation_counts(reloaded.iter());
    assert_eq!(
        counts.total,
        RuntimeObservationCounters {
            reports: 7,
            unsettled_reports: 3,
            local_failure_reports: 2,
            envelopes: 9,
            pending: 4,
            recorded: 2,
            refused: 1,
            local_failure: 2,
        }
    );
    assert_eq!(
        counts.by_wire.source_id,
        RuntimeObservationCounters {
            reports: 1,
            local_failure_reports: 1,
            envelopes: 3,
            pending: 1,
            refused: 1,
            local_failure: 1,
            ..Default::default()
        }
    );
    assert_eq!(
        counts.by_wire.source_free,
        RuntimeObservationCounters {
            reports: 1,
            unsettled_reports: 1,
            envelopes: 1,
            pending: 1,
            ..Default::default()
        }
    );
    assert_eq!(
        counts.by_wire.unknown,
        RuntimeObservationCounters {
            reports: 5,
            unsettled_reports: 2,
            local_failure_reports: 1,
            envelopes: 5,
            pending: 2,
            recorded: 2,
            local_failure: 1,
            ..Default::default()
        }
    );
    assert_eq!(counts, runtime_observation_counts(reports.iter()));
    assert!(
        serde_json::to_vec(&reloaded).unwrap() == bytes,
        "census must preserve retained requests and delivery"
    );

    // The public result has fixed field names and numeric leaves, even when
    // every retained request, receipt and error carries private fixture text.
    let output = serde_json::to_value(&counts).unwrap();
    let counter_keys = [
        "reports",
        "unsettledReports",
        "localFailureReports",
        "envelopes",
        "pending",
        "recorded",
        "refused",
        "localFailure",
    ];
    for counters in
        std::iter::once(&output["total"]).chain(output["byWire"].as_object().unwrap().values())
    {
        let counters = counters.as_object().unwrap();
        assert_eq!(counters.len(), counter_keys.len());
        assert!(
            counter_keys
                .iter()
                .all(|key| counters.get(*key).is_some_and(Value::is_u64)),
            "diagnostics may expose counters only"
        );
    }
    assert_eq!(output.as_object().unwrap().len(), 2);
    let wire_keys: Vec<_> = output["byWire"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(wire_keys, vec!["sourceFree", "sourceId", "unknown"]);
}

#[test]
fn known_occurrences_inspect_only_their_source_metadata_and_classify_unknown_shapes() {
    use RuntimeObservationWire::*;
    let cases = vec![
        (
            json!({"kind":"observed_check","check":{"source_basis":null}}),
            SourceFree,
        ),
        (
            json!({"kind":"observed_check","check":{"source_basis":legacy()}}),
            Unknown,
        ),
        (
            json!({"kind":"observed_check","check":{"source_basis":owner()}}),
            SourceId,
        ),
        (
            json!({"kind":"unadmitted_turn","observed_checks":[{}, {"source_basis":owner()}]}),
            SourceId,
        ),
        (
            json!({"kind":"unadmitted_turn","observed_checks":[{"source_basis":legacy()}, {"source_basis":owner()}]}),
            Unknown,
        ),
        (
            json!({"kind":"inter_turn_change","source_change":{"detection":"assumed_missing_baseline","sighting":{"source_basis":owner()}}}),
            SourceId,
        ),
        (
            json!({"kind":"source_sighting","source_basis":{"source_revision":"content-v1:old","source_id":owner()["source_id"]}}),
            Unknown,
        ),
        (
            json!({"kind":"source_sighting","source_basis":{"source_revision":"git-commit:old"}}),
            Unknown,
        ),
        (
            json!({"kind":"source_sighting","source_basis":{"source_revision":"git-commit:old","source_id":owner()["source_id"]}}),
            Unknown,
        ),
        (
            json!({"kind":"source_sighting","source_basis":{"source_revision":"content-v1:","source_id":owner()["source_id"]}}),
            Unknown,
        ),
        (
            json!({"kind":"source_sighting","source_basis":{"source_revision":"private-unknown","source_id":owner()["source_id"]}}),
            Unknown,
        ),
        (
            json!({"kind":"source_sighting","source_basis":{"source_revision":42}}),
            Unknown,
        ),
        (
            json!({"kind":"source_sighting","source_basis":{"source_id":"private-malformed"}}),
            Unknown,
        ),
        (json!({"kind":"source_sighting","source_basis":{}}), Unknown),
        (
            json!({"kind":"unadmitted_turn","observed_checks":"private-malformed"}),
            Unknown,
        ),
        (json!({"kind":"observed_check","check":null}), Unknown),
        (
            json!({"kind":"inter_turn_change","source_change":{"detection":"future"}}),
            Unknown,
        ),
        (json!({"kind":"observed_check"}), Unknown),
        (json!({"kind":"future"}), Unknown),
    ];
    for (occurrence, expected) in cases {
        let request = envelope(occurrence, pending()).request;
        assert!(
            runtime_observation_request_wire(&request) == expected,
            "wire classification differs"
        );
    }
    for request in [
        Value::Null,
        json!({"operation":"future","occurrence":{"kind":"observed_check","check":{}}}),
    ] {
        assert!(
            runtime_observation_request_wire(&request) == Unknown,
            "unknown request must stay unknown"
        );
    }
    let request = envelope(json!({"kind":"observed_check","check":{
        "host_evidence_ref":"source_id content-v1:private-text", "unrelated":{"source_id":owner()}
    }}), pending()).request;
    assert!(
        runtime_observation_request_wire(&request) == SourceFree,
        "free text is not source metadata"
    );
}

#[test]
fn real_builder_quiet_changed_turns_and_spillover_checks_preserve_custody() {
    let from = "2026-01-01T00:00:00Z";
    let through = "2026-01-01T00:00:02Z";
    let built_at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:03Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let basis = |key: &str| EngramUnadmittedObservationBasis {
        idempotency_key: key.into(),
        binding: EngramControlWorkBinding {
            root_execution_id: "fixture-root".into(),
            work_id: "fixture-work".into(),
            run_id: "fixture-run".into(),
            work_revision: 1,
            claim_id: "fixture-claim".into(),
            claim_fence: 1,
        },
        root_basis: EngramObservationRootBasis {
            capture_run_cut: 1,
            latest_event: None,
            state: EngramObservationRootState::None {},
        },
        observed_interval: EngramObservationInterval {
            from: from.into(),
            through: through.into(),
        },
        policy_basis: EngramObservationPolicyBasis::AccountIfEligible {
            project_policy_epoch: 1,
            policy: "a".repeat(32),
            obligation_rule_set: "b".repeat(32),
        },
    };
    for source in [legacy(), owner()] {
        let source_basis: EngramExecutionSourceBasis = serde_json::from_value(source).unwrap();
        let is_owner = source_basis.source_id.is_some();
        let mut changed_basis = source_basis.clone();
        if let Some(source) = &mut changed_basis.source_id {
            source.tree = "e".repeat(40);
        } else {
            changed_basis.source_revision = "content-v1:changed-revision".into();
        }
        let change = EngramObservedSourceChange::ContentComparison {
            workspace_id: source_basis.workspace_id.clone(),
            baseline: EngramMeasuredBaseline {
                workspace_id: source_basis.workspace_id.clone(),
                source_revision: source_basis.source_revision.clone(),
                source_id: source_basis.source_id.clone(),
                observed_at: from.into(),
            },
            sighting: EngramMeasuredSighting {
                source_basis: changed_basis,
                observed_at: through.into(),
            },
        };
        let requests = [
            engram_unadmitted_turn_request(
                basis("fixture-quiet-turn"),
                "fixture-turn".into(),
                None,
                vec![],
                built_at,
            )
            .unwrap(),
            engram_source_sighting_request(
                basis("fixture-sighting"),
                EngramMeasuredSighting {
                    source_basis,
                    observed_at: through.into(),
                },
                built_at,
            )
            .unwrap(),
            engram_observed_check_request(
                basis("fixture-check"),
                "fixture-turn".into(),
                EngramObservedCheck {
                    host_check_id: "fixture-check".into(),
                    check_kind: EngramVerificationKind::Test,
                    observed_result: EngramObservedCheckResult::Passed,
                    started_at: Some(from.into()),
                    finished_at: Some(through.into()),
                    observed_at: through.into(),
                    source_basis: None,
                    host_evidence_ref: None,
                },
                built_at,
            )
            .unwrap(),
            engram_unadmitted_turn_request(
                basis("fixture-changed-turn"),
                "fixture-turn".into(),
                Some(change),
                vec![],
                built_at,
            )
            .unwrap(),
        ];
        let envelopes: Vec<_> = requests
            .into_iter()
            .map(|request| EngramRuntimeObservationEnvelope {
                request: serde_json::to_value(request).unwrap(),
                delivery: pending(),
            })
            .collect();
        for reversed in [false, true] {
            let mut retained = report(envelopes.clone());
            if reversed {
                retained.envelopes.reverse();
            }
            let before = serde_json::to_vec(&retained).unwrap();
            let counts = runtime_observation_counts(std::iter::once(&retained));
            assert_eq!(counts.total.reports, 1);
            assert_eq!(counts.total.envelopes, 4);
            assert_eq!(counts.by_wire.unknown.reports, u64::from(!is_owner));
            assert_eq!(counts.by_wire.source_id.reports, u64::from(is_owner));
            assert_eq!(counts.by_wire.unknown.envelopes, 2 * u64::from(!is_owner));
            assert_eq!(counts.by_wire.source_id.envelopes, 2 * u64::from(is_owner));
            assert_eq!(counts.by_wire.source_free.envelopes, 2);
            assert_eq!(counts.by_wire.source_free.reports, 0);
            assert!(
                serde_json::to_vec(&retained).unwrap() == before,
                "census changed real builder requests"
            );
            retained
                .envelopes
                .push(envelope(json!({"kind":"future-occurrence"}), recorded()));
            let counts = runtime_observation_counts(std::iter::once(&retained));
            assert_eq!(counts.by_wire.unknown.reports, 1);
            assert_eq!(counts.by_wire.source_id.reports, 0);
        }
    }
}

#[tokio::test]
async fn host_surface_counts_all_resident_sessions_after_host_reload_without_touching_delivery() {
    let state = crate::tests::test_app_state();
    let reports = mixed_reports();
    let session_ids = {
        let mut inner = state.inner.lock().unwrap();
        inner.create_session(
            Agent::Codex,
            None,
            state.default_workdir.clone(),
            None,
            None,
        );
        inner.create_session(
            Agent::Claude,
            None,
            state.default_workdir.clone(),
            None,
            None,
        );
        inner.sessions[0].engram.unadmitted_reports = reports[..3].to_vec();
        inner.sessions[1].engram.unadmitted_reports = reports[3..].to_vec();
        [
            inner.sessions[0].session.id.clone(),
            inner.sessions[1].session.id.clone(),
        ]
    };
    let Json(first) = get_runtime_observation_counts(State(state.clone()))
        .await
        .unwrap();
    {
        let inner = state.inner.lock().unwrap();
        persist_state(state.persistence_path.as_path(), &inner).unwrap();
    }
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        *inner = loaded;
    }
    // The second handler call and routed GET now read the host's actual
    // reloaded sessions, not a stand-alone serde copy of the reports.
    let Json(second) = get_runtime_observation_counts(State(state.clone()))
        .await
        .unwrap();
    assert_eq!(first, runtime_observation_counts(reports.iter()));
    assert_eq!(first, second);
    let response = app_router(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/diagnostics/runtime-observations")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body == serde_json::to_value(&first).unwrap(),
        "response differs from the counter snapshot"
    );
    let response = app_router(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/diagnostics/runtime-observations")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    let inner = state.inner.lock().unwrap();
    let first_session = inner.find_session_index(&session_ids[0]).unwrap();
    let second_session = inner.find_session_index(&session_ids[1]).unwrap();
    assert!(
        inner.sessions[first_session].engram.unadmitted_reports == reports[..3],
        "first session delivery changed"
    );
    assert!(
        inner.sessions[second_session].engram.unadmitted_reports == reports[3..],
        "second session delivery changed"
    );
    assert!(
        reports[1].is_settled() && !reports[1].is_discharged(),
        "local failure is not a producer answer"
    );
}
