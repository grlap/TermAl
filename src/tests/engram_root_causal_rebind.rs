//! Value-safe decoding on the remaining ordinary work-service boundary.
use super::*;

#[test]
fn ordinary_work_envelope_validation_hides_received_values() {
    let sentinel = "opaque-response-sentinel-73f9";
    let error = exchange_engram_control_frame(&mut Vec::new(),
        &mut std::io::Cursor::new(format!("{{\"status\":\"{sentinel}\"}}\n").into_bytes()), b"{}").unwrap_err();
    let request = EngramControlRequest::WorkSessionRead {};
    let cause = EngramCausalFailure::request(&error, &request);
    let encoded = serde_json::to_string(&cause).unwrap();
    assert_eq!(error.kind, EngramTransportErrorKind::Protocol);
    assert!(!encoded.contains(sentinel));
    assert_eq!(cause.operation, "work_session_read");
    assert!(cause.attempt_id.is_none(), "a read has no fabricated idempotency key");
    assert_eq!(cause.remote_application, EngramRemoteApplication::Unknown);
    assert!(cause.message.contains("names withheld"));
}

#[test]
fn ordinary_held_claim_decode_hides_dynamic_keys_and_capability_values() {
    let secret = "opaque-response-capability-73f9";
    let error = parse_engram_result::<EngramHeldClaims>(json!({"items":{secret:secret}})).unwrap_err();
    let cause = EngramCausalFailure::request(&error, &EngramControlRequest::WorkSessionRead {});
    for text in [&error.message, &serde_json::to_string(&cause).unwrap()] {
        assert!(!text.contains(secret), "received values never enter diagnostics");
    }
    assert_eq!(cause.failure_class, EngramCausalFailureClass::Protocol);
    assert_eq!(cause.boundary, "Engram control response validation");
    assert!(cause.message.contains("kind=Data; field path=$"));
}

#[test]
fn root_causal_result_validation_path_never_discloses_map_keys_or_values() {
    let key = "opaque-map-key-secret-72a1";
    let value = "opaque-malformed-value-secret-35b8";
    let error = parse_engram_result::<Vec<HashMap<String, u64>>>(json!([{key: value}]))
        .unwrap_err();
    assert_eq!(error.kind, EngramTransportErrorKind::Protocol);
    assert_eq!(error.message,
        "invalid Engram result schema: kind=Data; field path=$[0][field] (names withheld)");
    assert!(!error.message.contains(key));
    assert!(!error.message.contains(value));
    let valid = parse_engram_result::<Vec<HashMap<String, u64>>>(json!([{key: 7}]))
        .unwrap();
    assert_eq!(valid[0][key], 7, "accepted values and dynamic map keys are unchanged");
}
