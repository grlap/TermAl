//! Invocation-only non-application at admission and exact Begin replay.
//! A remote certificate never resolves an earlier attempt or a local timeout.

use super::*;

const CERTIFIED_CODES: [&str; 2] = ["control_request_expired", "control_store_busy"];

fn requests_for(transport: &ScriptedEngramControlTransport, operation: &str) -> Vec<Value> {
    transport
        .requests()
        .into_iter()
        .filter(|sent| sent.request["operation"] == operation)
        .map(|sent| sent.request)
        .collect()
}

#[test]
fn control_non_application_evaluate_automatically_replays_retained_request() {
    for code in CERTIFIED_CODES {
        let (state, session, receiver, transport) = root_fixture([
            bind_reply("retained-token"),
            remote_error_reply(code),
            grant_reply("retained-grant"),
            begin_reply("retained-grant"),
        ]);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        assert!(receiver.try_recv().is_err());
        let retry = admission_retry(&state, &session)
            .unwrap_or_else(|| panic!("{code}: automatic retry, not a Reconcile hold"));
        assert_eq!(retry.code, code);
        tick_past_due(&state, &session);
        assert_eq!(abort_retry::prompts_received(&receiver), 1);
        let requests = requests_for(&transport, "turn_evaluate");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1], requests[0],
            "{code}: exact payload and idempotency key"
        );
        assert_eq!(requests_for(&transport, "session_bind").len(), 1);
        assert!(admission_retry(&state, &session).is_none());
        eprintln!("certified invocation replay: {code} evaluate reused its exact request");
    }
}

#[test]
fn control_non_application_bind_replays_its_original_request() {
    for code in CERTIFIED_CODES {
        let (state, session, receiver, transport) = root_fixture([
            remote_error_reply(code),
            bind_reply("retained-token"),
            grant_reply("retained-grant"),
            begin_reply("retained-grant"),
        ]);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        assert!(receiver.try_recv().is_err());
        tick_past_due(&state, &session);
        assert_eq!(
            abort_retry::prompts_received(&receiver),
            1,
            "{code}: retained bind resumes"
        );
        let requests = requests_for(&transport, "session_bind");
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1], requests[0],
            "{code}: no replacement binding request"
        );
        eprintln!("certified invocation replay: {code} bind reused its exact request");
    }
}

fn assert_begin_replays(code: &str, earlier_unknown: bool) {
    let mut replies = vec![bind_reply("retained-token"), grant_reply("retained-grant")];
    if earlier_unknown {
        replies.push(ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::deadline("earlier Begin may already have applied"),
        )));
    }
    replies.extend([remote_error_reply(code), begin_reply("retained-grant")]);
    let (state, session, receiver, transport) = root_fixture(replies);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    if earlier_unknown {
        tick_past_due(&state, &session);
    }
    assert!(receiver.try_recv().is_err());
    with_record(&state, &session, |record| {
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some("retained-grant")
        );
        assert!(
            record.queued_prompts[0]
                .engram_evaluate
                .as_ref()
                .unwrap()
                .prepared_begin
                .is_some()
        );
        assert!(
            record.engram.admission_retry.is_some(),
            "{code}: exact Begin replay stays scheduled"
        );
    });
    tick_past_due(&state, &session);
    assert_eq!(abort_retry::prompts_received(&receiver), 1);
    let begins = requests_for(&transport, "turn_begin");
    assert_eq!(begins.len(), if earlier_unknown { 3 } else { 2 });
    assert!(
        begins.iter().all(|request| request == &begins[0]),
        "{code}: same Begin, grant and key"
    );
    assert_eq!(
        requests_for(&transport, "turn_evaluate").len(),
        1,
        "no fresh authorization"
    );
    assert_eq!(
        requests_for(&transport, "session_bind").len(),
        1,
        "same control binding"
    );
    eprintln!(
        "certified invocation replay: {code} begin reused its key; earlier_unknown={earlier_unknown}"
    );
}

#[test]
fn control_non_application_begin_replays_the_prepared_key() {
    for code in CERTIFIED_CODES {
        assert_begin_replays(code, false);
    }
}

#[test]
fn control_non_application_begin_preserves_earlier_unknown_application() {
    for code in CERTIFIED_CODES {
        assert_begin_replays(code, true);
    }
}

#[test]
fn control_non_application_remote_certificate_does_not_mean_unsent() {
    for code in CERTIFIED_CODES {
        let error = EngramTransportError::remote(EngramControlErrorBody {
            code: code.to_owned(),
            message: "this invocation committed no changes".to_owned(),
        });
        assert!(!error.process_never_started);
        assert!(error.keeps_control_process_alive());
        assert!(!error.disables_session());
        assert!(!error.counts_for_circuit_breaker());
        let cause = EngramCausalFailure::error(&error, "turn_begin", Some("retained-key"), &[]);
        let wire = serde_json::to_value(cause).unwrap();
        assert_eq!(
            wire["remoteApplication"], "not_applied",
            "{code}: invocation-only certificate"
        );
        assert_eq!(wire["failureClass"], "remote");
        eprintln!("certified invocation replay: {code} is not_applied, never unsent");
    }
}

#[test]
fn control_non_application_never_certifies_a_local_deadline_or_storage_error() {
    let deadline = EngramTransportError::deadline("response not observed before host deadline");
    assert_eq!(deadline.code.as_deref(), Some("deadline_exceeded"));
    assert!(!deadline.process_never_started);
    assert!(deadline.counts_for_circuit_breaker());
    assert!(!deadline.keeps_control_process_alive());
    let unknown = EngramCausalFailure::error(&deadline, "turn_begin", Some("retained-key"), &[]);
    assert_eq!(unknown.remote_application, EngramRemoteApplication::Unknown);
    for code in CERTIFIED_CODES {
        let mut local =
            EngramTransportError::deadline("a local error cannot certify a remote outcome");
        local.code = Some(code.to_owned());
        assert_eq!(
            EngramCausalFailure::error(&local, "turn_begin", None, &[]).remote_application,
            EngramRemoteApplication::Unknown,
            "the code alone is insufficient"
        );
        let mut remote = EngramTransportError::remote(EngramControlErrorBody {
            code: code.to_owned(),
            message: "only this invocation did not apply".to_owned(),
        });
        remote.causal_failure = Some(unknown.clone());
        let preserved =
            EngramCausalFailure::error(&remote, "turn_begin", Some("retained-key"), &[]);
        assert_eq!(
            preserved, unknown,
            "{code}: preserve an earlier unresolved causal boundary"
        );
    }
    let storage = EngramTransportError::remote(EngramControlErrorBody {
        code: "storage_error".to_owned(),
        message: "COMMIT or rollback failed".to_owned(),
    });
    assert_eq!(
        EngramCausalFailure::error(&storage, "turn_checkpoint", None, &[]).remote_application,
        EngramRemoteApplication::Unknown
    );
}

#[test]
fn control_non_application_legacy_causal_records_remain_readable() {
    let deadline = EngramTransportError::deadline("legacy retained failure");
    for application in [
        EngramRemoteApplication::Unknown,
        EngramRemoteApplication::NotStarted,
        EngramRemoteApplication::Refused,
    ] {
        let mut original = EngramCausalFailure::error(
            &deadline,
            "turn_checkpoint",
            Some("legacy-retained-key"),
            &[],
        );
        original.remote_application = application;
        let mut legacy = serde_json::to_value(&original).unwrap();
        // This optional metadata was absent from earlier persisted cards.
        legacy.as_object_mut().unwrap().remove("controlProcess");
        let restored: EngramCausalFailure = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            restored, original,
            "existing causal values and missing metadata keep their meaning"
        );
    }
}

#[test]
fn control_non_application_recovery_checkpoint_keeps_report_admission_distinct() {
    for code in CERTIFIED_CODES {
        let response: std::result::Result<EngramTurnCheckpointResponse, EngramTransportError> =
            Err(EngramTransportError::remote(EngramControlErrorBody {
                code: code.to_owned(),
                message: "no new changes in this invocation".to_owned(),
            }));
        assert_eq!(
            engram_recovery_checkpoint_refusal(&response),
            None,
            "{code}: a recovery closer must keep the report for its exact retry"
        );
    }
    let refusal: std::result::Result<EngramTurnCheckpointResponse, EngramTransportError> =
        Err(EngramTransportError::remote(EngramControlErrorBody {
            code: "summary_redacted".to_owned(),
            message: "report payload refused".to_owned(),
        }));
    assert_eq!(
        engram_recovery_checkpoint_refusal(&refusal),
        Some(Some("summary_redacted")),
        "an actual report refusal retains its existing fallback policy"
    );
}
