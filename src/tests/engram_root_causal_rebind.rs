//! Malformed rebind responses cannot disclose payload values or lose attribution.
//! Uses the existing queued path and connected writer, not live control stores.

use super::*;

fn queued_rebind_validation(operation: &'static str, ordinal: usize, mode: CausalFaultMode) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some((operation, ordinal)));
    *fixture.transport.mode.lock().unwrap() = mode;
    let (token, original_head, generation) = fixture.start_and_queue();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.session_mut_by_index(index).unwrap().engram.rebind_required = true;
    }
    let _completion = fixture.state.finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    let request = fixture.transport.lost_request.lock().unwrap().clone()
        .expect("queued rebind must reach the selected malformed response");
    assert_eq!(request["operation"], operation);
    let cards = fixture.durable_cards();
    let count = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.session).unwrap()].session.messages.len()
    };
    let persisted = serde_json::to_value(load_persisted_message_range(
        &fixture.state.persistence_path, &fixture.session, 0, count,
    ).unwrap()).unwrap().to_string();
    let (wire, head, current_generation, promoted) = {
        let inner = fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
        (
            serde_json::to_value(AppState::wire_session_from_record(
                &fixture.state.server_instance_id, record,
            )).unwrap().to_string(),
            record.queued_prompts.front()
                .map(|queued| serde_json::to_value(&queued.pending_prompt).unwrap()),
            record.engram.dispatch_generation,
            record.session.messages.iter().find_map(|message| match message {
                Message::Text { id, author: Author::You, .. }
                    if Some(id.as_str()) == original_head["id"].as_str() =>
                {
                    Some(serde_json::to_value(message).unwrap())
                }
                _ => None,
            }),
        )
    };
    // Existing dispatch-card handling retires a promoted head when a protocol
    // failure precedes prepared bind/evaluate intent. It does not retain wire
    // intent that was never created; the original prompt remains in history.
    // Snapshot under StateInner, but assert only after releasing it so a RED
    // cannot poison the writer and abort again during fixture cleanup.
    assert_eq!(head, None, "unprepared promoted head is retired by existing policy");
    assert_eq!(current_generation, generation + 1, "one ordinary promotion");
    let promoted = promoted.expect("original promoted prompt remains in transcript");
    for field in ["id", "text", "expandedText", "source"] {
        assert_eq!(promoted[field], original_head[field], "prompt field {field}");
    }
    assert!(fixture.receiver.try_recv().is_err(), "malformed rebind cannot hand off to provider");
    for projection in [&persisted, &wire] {
        assert!(!projection.contains(request["routing_token"].as_str().unwrap()),
            "malformed reply capability leaked into persisted/public record");
        assert!(!projection.contains("opaque-response-sentinel-73f9"),
            "raw malformed payload value leaked into persisted/public record");
    }
    let cause = &cards.last().unwrap()["causalFailure"];
    assert_eq!(cause["operation"], operation);
    assert_eq!(cause["attemptId"], request["idempotency_key"]);
    assert_eq!(cause["boundary"], "Engram control response validation");
    assert_eq!(cause["failureClass"], "protocol");
    assert_eq!(cause["remoteApplication"], "unknown");
    assert!(cause["message"].as_str().unwrap().contains("kind=Data; field path=$"));
}

#[test]
fn root_causal_rebind_status_validation_hides_payload_values() {
    queued_rebind_validation("session_status", 1, CausalFaultMode::MalformedRequestSecret);
}

#[test]
fn root_causal_rebind_checkpoint_validation_hides_payload_values() {
    queued_rebind_validation("turn_checkpoint", 2, CausalFaultMode::RecoveryMalformedRequestSecret);
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
