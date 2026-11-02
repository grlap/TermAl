//! Historical causal records remain data; no retired request is reconstructed or sent.
use super::*;

#[test]
fn historical_causal_records_preserve_unknown_application_without_process_metadata() {
    for application in ["unknown", "not_started", "refused"] {
        let historical = json!({
            "operation": "turn_checkpoint",
            "failureClass": "deadline",
            "originalCode": "deadline_exceeded",
            "message": "Historical retained failure\nżółw 🐢",
            "boundary": "reply not observed",
            "attemptId": "original-attempt-key",
            "remoteApplication": application,
            "continuationReason": "Original application remains unconfirmed"
        });
        let restored: EngramCausalFailure = serde_json::from_value(historical.clone()).unwrap();
        let mut encoded = serde_json::to_value(restored).unwrap();
        let process = encoded.as_object_mut().unwrap().remove("controlProcess").unwrap();
        assert_eq!(process, json!({"identity": "unavailable", "exitState": "unavailable"}));
        assert_eq!(encoded, historical, "missing process metadata cannot rewrite history");
    }
}
