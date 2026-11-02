//! Remaining decoder diagnostics and historical original facts, without gate recovery.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;
use crate::tests::engram_host_adapter::turn_checks::checkpoint_result_recovery::{
    passive_checkpoint, install_historical_checkpoint, append_historical_result,
};

#[test]
fn ordinary_envelope_decode_keeps_received_values_out_of_causal_metadata() {
    let sentinel = "opaque-envelope-sentinel-5d21";
    let error = exchange_engram_control_frame(&mut Vec::new(),
        &mut std::io::Cursor::new(format!("{{\"status\":\"{sentinel}\"}}\n").into_bytes()), b"{}").unwrap_err();
    let cause = EngramCausalFailure::request(&error, &EngramControlRequest::WorkSessionRead {});
    let wire = serde_json::to_value(&cause).unwrap();
    assert!(!wire.to_string().contains(sentinel));
    assert_eq!(wire["operation"], "work_session_read");
    assert_eq!(wire["failureClass"], "protocol");
    assert_eq!(wire["remoteApplication"], "unknown");
    assert!(wire["attemptId"].is_null());
    let message = wire["message"].as_str().unwrap();
    assert!(message.starts_with("invalid Engram response: kind="));
    assert!(message.ends_with("(names withheld)"));
}

#[test]
fn ordinary_work_result_schema_never_echoes_a_returned_capability() {
    let capability = "opaque-phase-sentinel-91e4";
    let error = parse_engram_result::<EngramHeldClaims>(json!({"items":[{
        "work_id":capability, "claim_fence":capability
    }]})).unwrap_err();
    let cause = EngramCausalFailure::request(&error, &EngramControlRequest::WorkSessionRead {});
    assert!(!serde_json::to_string(&cause).unwrap().contains(capability));
    assert!(!error.message.contains(capability));
    assert_eq!(cause.failure_class, EngramCausalFailureClass::Protocol);
    assert_eq!(cause.remote_application, EngramRemoteApplication::Unknown);
}

#[test]
fn historical_checkpoint_original_and_earlier_unknown_survive_a_later_refusal() {
    let turn = OrdinaryRoot::new("historical-receipt-refusal-data");
    let mut operation = passive_checkpoint(&turn.root, &turn.session);
    let original = serde_json::to_value(&operation.original).unwrap();
    let first = serde_json::to_value(&operation.attempts[0]).unwrap();
    append_historical_result(&mut operation, EngramCheckpointAttemptResult::Refused {
        response: json!({"decision":"refuse","code":"lifecycle_hold"}),
    });
    operation.validate().unwrap();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        install_historical_checkpoint(&mut inner, &turn.session, &operation);
        turn.state.commit_locked(&mut inner).unwrap();
    }
    deliver_turn_dispatch(&turn.state, turn.prepare("Independent ordinary input.")).into_public_result().unwrap();
    turn.prompt();
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let kept = &loaded.engram_session_obligations.checkpoint_operations[0];
    assert_eq!(serde_json::to_value(&kept.original).unwrap(), original);
    assert_eq!(serde_json::to_value(&kept.attempts[0]).unwrap(), first);
    let raw = serde_json::to_value(kept).unwrap();
    assert_eq!(raw["attempts"][1]["result"]["response"]["code"], "lifecycle_hold");
    assert_ne!(kept.protection.phase, EngramSessionObligationPhase::Confirmed);
    assert!(kept.protection.settlement_evidence.is_null());
    assert!(loaded.test_run_observations.is_empty());
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn loading_unknown_original_keeps_exact_content_without_requesting_a_status_or_checkpoint() {
    let turn = OrdinaryRoot::new("historical-cold-original");
    let id = turn.enqueue("Original unknown input 🦀", QueuedPromptSource::User);
    let before = turn.edit(|record| {
        let head = &mut record.queued_prompts[0];
        head.engram_evaluate = Some(json!({"request":{"operation":"turn_evaluate",
            "idempotency_key":"original-unknown-key"},"prepared_begin":{"grant_id":"old-grant"}}));
        head.engram_ordinary_handoff = true;
        sync_pending_prompts(record);
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    });
    let loaded = turn.record(|record| PersistedSessionRecord::from_record(record).into_record().unwrap());
    assert_eq!(loaded.queued_prompts[0].pending_prompt.id, id);
    assert_eq!(serde_json::to_value(&loaded.queued_prompts[0]).unwrap(), before);
    assert!(ordinary_queue_position(&loaded).is_none());
    assert!(loaded.prompt_deliveries.is_empty());
    turn.state.resume_session_queue(&turn.session).unwrap();
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn root_causal_decoders_keep_received_text_out_of_messages() {
    let sentinel = "opaque-decode-sentinel-0e47";
    let held = engram_decode_value::<EngramHeldClaims>(
        json!({"items": sentinel}),
        "invalid Engram work core held output",
    )
    .unwrap_err();
    let trailing = engram_decode_slice::<Value>(
        format!("{{\"a\":1}} {sentinel}").as_bytes(),
        "invalid Engram work show response",
    )
    .unwrap_err();
    let envelope = exchange_engram_control_frame(
        &mut Vec::new(),
        &mut std::io::Cursor::new(format!("{{\"status\":\"{sentinel}\"}}\n").into_bytes()),
        b"{}",
    )
    .unwrap_err();
    for (error, label) in [
        (held, "invalid Engram work core held output: kind="),
        (trailing, "invalid Engram work show response: kind="),
        (envelope, "invalid Engram response: kind="),
    ] {
        assert_eq!(error.kind, EngramTransportErrorKind::Protocol);
        assert!(error.message.starts_with(label), "{}", error.message);
        assert!(!error.message.contains(sentinel), "received text in {label}");
    }
    assert_eq!(
        engram_decode_slice::<Value>(b"{\"a\":1}\n", "accepted").unwrap(),
        json!({"a": 1}),
        "accepted input and trailing whitespace are unchanged"
    );
}
