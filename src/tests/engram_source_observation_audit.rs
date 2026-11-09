//! Receipt scope and historical recording never grant execution authority.
use super::*;

#[test]
fn ordinary_observation_audit_rejects_receipts_with_wrong_scope_policy_or_credit() {
    let fixture = ObservationSession::new();
    let mut trace = observation_trace();
    trace.commands.push(observation_command("check-one"));
    let report = EngramRuntimeObservationReport::prepare(
        &trace,
        observation_capture(),
        observation_connection(FsPath::new(&fixture.state.default_workdir)),
        observation_store(FsPath::new(&fixture.state.default_workdir)),
        "fixture-observer".into(),
        Duration::from_secs(1),
    )
    .unwrap();
    let request = report_observation(&report, 0);
    let valid = observation_receipt(&request, &report.observing_session);
    assert!(
        EngramSourceObservationReceipt::from_unadmitted_result(
            valid.clone(),
            &request,
            &report.observing_session
        )
        .is_ok()
    );
    for mode in [
        "claim",
        "run",
        "observer",
        "policy",
        "admission",
        "credit",
        "missing_check",
        "extra_check",
        "timestamp",
        "accounting",
        "unknown_reason",
    ] {
        let mut receipt = valid.clone();
        match mode {
            "claim" => receipt["binding"]["claim_fence"] = json!(99),
            "run" => receipt["position"]["feed"]["id"] = json!("other-run"),
            "observer" => receipt["observing_session"] = json!("other-observer"),
            "policy" => receipt["policy_basis"]["project_policy_epoch"] = json!(99),
            "admission" => receipt["admission"] = json!("admitted"),
            "credit" => receipt["observed_checks"][0]["credit"] = json!("credited"),
            "missing_check" => receipt["observed_checks"] = json!([]),
            "extra_check" => receipt["observed_checks"]
                .as_array_mut()
                .unwrap()
                .push(json!({"host_check_id": "other", "credit": "uncredited"})),
            "timestamp" => receipt["recorded_at"] = json!(observation_at(49)),
            "accounting" => receipt["accounting"] = json!({"kind": "no_source_change"}),
            "unknown_reason" => {
                receipt["accounting"] = json!({"kind": "audit_only", "reason": "explicit_audit"})
            }
            _ => unreachable!(),
        }
        let transport = Arc::new(ObservationReplies::default());
        transport.replies.lock().unwrap().push_back(Ok(receipt));
        let adapter = observation_adapter(
            transport.clone(),
            FsPath::new(&fixture.state.default_workdir),
        );
        let next =
            deliver_runtime_observation_envelope(&adapter, &report, 0, Duration::from_secs(1))
                .unwrap();
        assert!(
            matches!(
                next.envelopes[0].delivery,
                EngramRuntimeObservationDelivery::Pending {
                    last_error: Some(_)
                }
            ),
            "accepted {mode}"
        );
        assert_eq!(
            next.envelopes[0].request, report.envelopes[0].request,
            "invalid receipt must leave retry bytes intact: {mode}"
        );
        assert_eq!(transport.sent.lock().unwrap().len(), 1);
    }
}

#[test]
fn ordinary_observation_historical_receipt_records_without_promoting_source_or_claim() {
    for reason in ["root_basis_moved", "historical_binding", "finished_run"] {
        let fixture = ObservationSession::new();
        let report = fixture.report();
        let request = report_observation(&report, 0);
        let mut receipt = observation_receipt(&request, &report.observing_session);
        receipt["accounting"] = json!({"kind": "audit_only", "reason": reason});
        assert!(
            EngramSourceObservationReceipt::from_unadmitted_result(
                receipt.clone(),
                &request,
                &report.observing_session
            )
            .is_ok()
        );
        let transport = Arc::new(ObservationReplies::default());
        transport
            .replies
            .lock()
            .unwrap()
            .push_back(Ok(receipt.clone()));
        let adapter = observation_adapter(transport, FsPath::new(&fixture.state.default_workdir));
        let next =
            deliver_runtime_observation_envelope(&adapter, &report, 0, Duration::from_secs(1))
                .unwrap();
        fixture.retain(report.clone());
        fixture
            .state
            .install_runtime_observation_result(&fixture.id, &report, next, 0)
            .unwrap();
        fixture.record(|record| {
            assert!(record.engram.runtime_checkpoint.is_none());
            assert!(record.engram.work_binding.is_none());
            assert!(matches!(&record.engram.unadmitted_reports[0].envelopes[0].delivery,
                EngramRuntimeObservationDelivery::Recorded { receipt: actual } if actual == &receipt));
        });
    }
}

#[test]
fn ordinary_observation_invalid_retained_wire_is_not_repaired_or_sent() {
    let fixture = ObservationSession::new();
    for corruption in ["unknown_field", "wrong_kind", "bad_index"] {
        let mut report = fixture.report();
        match corruption {
            "unknown_field" => report.envelopes[0].request["unsupported_field"] = json!(true),
            "wrong_kind" => report.envelopes[0].request["operation"] = json!("not_an_observation"),
            "bad_index" => {}
            _ => unreachable!(),
        }
        let transport = Arc::new(ObservationReplies::default());
        let adapter = observation_adapter(
            transport.clone(),
            FsPath::new(&fixture.state.default_workdir),
        );
        let index = if corruption == "bad_index" {
            report.envelopes.len()
        } else {
            0
        };
        assert!(
            deliver_runtime_observation_envelope(&adapter, &report, index, Duration::from_secs(1))
                .is_err()
        );
        assert!(transport.sent.lock().unwrap().is_empty());
    }
}
