//! Received values never reach the committed causal record or the wire.
//! Owns the envelope, bind-phase, recovery-receipt and cold-restore vectors,
//! not recovery decisions. Split beside engram_root_causal_rebind.rs and
//! registered by engram_root_causal_failure.rs, whose fixture it uses.

use super::*;

struct Committed {
    cards: Vec<Value>,
    projections: [String; 2],
    head: Option<Value>,
}

// Nothing may panic while StateInner is held: a RED would poison the lock and
// abort again during fixture cleanup. These read or change the session under
// the guard and leave every unwrap and assertion to the caller, after release.
fn read_record<T>(fixture: &CausalRoot, read: impl FnOnce(&SessionRecord) -> T) -> T {
    let value = {
        let inner = fixture.state.inner.lock().unwrap();
        inner
            .find_session_index(&fixture.session)
            .map(|index| read(&inner.sessions[index]))
    };
    value.expect("the fixture session exists")
}

fn write_record(fixture: &CausalRoot, write: impl FnOnce(&mut SessionRecord)) {
    let found = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session);
        index.and_then(|index| inner.session_mut_by_index(index)).map(write)
    };
    found.expect("the fixture session exists");
}

fn committed(fixture: &CausalRoot) -> Committed {
    let cards = fixture.durable_cards();
    let count = read_record(fixture, |record| record.session.messages.len());
    let persisted = serde_json::to_value(
        load_persisted_message_range(&fixture.state.persistence_path, &fixture.session, 0, count)
            .unwrap(),
    )
    .unwrap()
    .to_string();
    let (wire, head) = read_record(fixture, |record| {
        (
            serde_json::to_value(AppState::wire_session_from_record(
                &fixture.state.server_instance_id,
                record,
            )),
            record
                .queued_prompts
                .front()
                .map(|queued| serde_json::to_value(&queued.pending_prompt)),
        )
    });
    let wire = wire.unwrap().to_string();
    let head = head.transpose().unwrap();
    Committed { cards, projections: [persisted, wire], head }
}

fn assert_absent(committed: &Committed, values: &[&str]) {
    for projection in &committed.projections {
        for value in values {
            assert!(
                !projection.contains(value),
                "a received value leaked into the persisted/public record"
            );
        }
    }
}

fn lost_request(fixture: &CausalRoot) -> Value {
    fixture
        .transport
        .lost_request
        .lock()
        .unwrap()
        .clone()
        .expect("the selected malformed response must be reached")
}

fn last_cause(committed: &Committed) -> Value {
    committed.cards.last().expect("a committed card")["causalFailure"].clone()
}

#[test]
fn root_causal_envelope_decode_keeps_received_values_out_of_the_record() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_evaluate", 2)));
    *fixture.transport.mode.lock().unwrap() = CausalFaultMode::MalformedEnvelopeSecret;
    let (token, original_head, _) = fixture.start_and_queue();
    let _completion = fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    let request = lost_request(&fixture);
    let committed = committed(&fixture);
    assert_eq!(
        committed.head.as_ref(),
        Some(&original_head),
        "existing decision: the unresolved request retains its exact prompt"
    );
    assert!(fixture.receiver.try_recv().is_err(), "no provider handoff");
    assert_absent(
        &committed,
        &[
            "opaque-envelope-sentinel-5d21",
            request["routing_token"].as_str().unwrap(),
        ],
    );
    let cause = last_cause(&committed);
    assert_eq!(cause["operation"], "turn_evaluate");
    assert_eq!(cause["attemptId"], request["idempotency_key"]);
    assert_eq!(cause["failureClass"], "protocol");
    assert_eq!(cause["boundary"], "Engram control response validation");
    assert_eq!(cause["remoteApplication"], "unknown");
    let message = cause["message"].as_str().unwrap();
    assert!(message.starts_with("invalid Engram response: kind="), "{message}");
    assert!(message.ends_with("(names withheld)"), "{message}");
}

#[test]
fn root_causal_rebind_phase_keeps_returned_capability_out_of_the_record() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("session_bind", 2)));
    *fixture.transport.mode.lock().unwrap() = CausalFaultMode::RebindPhaseSecret;
    let (token, _, _) = fixture.start_and_queue();
    write_record(&fixture, |record| record.engram.rebind_required = true);
    let _completion = fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    let request = lost_request(&fixture);
    let echoed = fixture
        .transport
        .echoed
        .lock()
        .unwrap()
        .clone()
        .expect("the producer returned a capability");
    let committed = committed(&fixture);
    assert_eq!(request["operation"], "session_bind");
    assert!(fixture.receiver.try_recv().is_err(), "no provider handoff");
    assert_absent(&committed, &["opaque-phase-sentinel-91e4", &echoed]);
    let cause = last_cause(&committed);
    assert_eq!(cause["operation"], "session_bind");
    assert_eq!(cause["attemptId"], request["idempotency_key"]);
    assert_eq!(cause["failureClass"], "protocol");
    assert_eq!(cause["boundary"], "Engram control response validation");
    assert_eq!(
        cause["message"],
        "Engram rebind returned a phase instead of `ready` or `sync_required`: \
         field path=$.status.phase (value withheld)"
    );
}

#[test]
fn root_causal_recovery_receipt_keeps_received_grant_out_of_the_record() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_checkpoint", 2)));
    *fixture.transport.mode.lock().unwrap() = CausalFaultMode::RecoveryReceiptSecret;
    let (token, _, _) = fixture.start_and_queue();
    write_record(&fixture, |record| record.engram.rebind_required = true);
    let _completion = fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    let request = lost_request(&fixture);
    let committed = committed(&fixture);
    assert_eq!(request["operation"], "turn_checkpoint");
    assert!(fixture.receiver.try_recv().is_err(), "no provider handoff");
    assert_absent(
        &committed,
        &[
            "opaque-receipt-sentinel-6b0d",
            request["routing_token"].as_str().unwrap(),
        ],
    );
    let cause = last_cause(&committed);
    assert_eq!(cause["operation"], "turn_checkpoint");
    assert_eq!(cause["attemptId"], request["idempotency_key"]);
    assert_eq!(cause["failureClass"], "remote");
    assert_eq!(cause["originalCode"], "restart_checkpoint_receipt_mismatch");
    assert_eq!(cause["remoteApplication"], "unknown");
}

#[test]
fn root_causal_recovery_refusal_keeps_the_producer_code() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_checkpoint", 2)));
    *fixture.transport.mode.lock().unwrap() = CausalFaultMode::RecoveryRefusal;
    let (token, _, _) = fixture.start_and_queue();
    write_record(&fixture, |record| record.engram.rebind_required = true);
    let _completion = fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    let request = lost_request(&fixture);
    let committed = committed(&fixture);
    assert_eq!(request["operation"], "turn_checkpoint");
    assert!(fixture.receiver.try_recv().is_err(), "no provider handoff");
    let cause = last_cause(&committed);
    assert_eq!(cause["operation"], "turn_checkpoint");
    assert_eq!(cause["attemptId"], request["idempotency_key"]);
    assert_eq!(cause["originalCode"], "lifecycle_hold", "the producer's own code");
    assert_eq!(cause["failureClass"], "producer_refusal");
    assert_eq!(cause["remoteApplication"], "refused");
}

// A lost evaluate reply leaves its prepared evaluate on the exact head; the
// saved record then replaces the live one, as the loader does after a restart.
// Then selects the restore's own malformed reply. Returns the original head.
fn cold_restored(
    fixture: &CausalRoot,
    operation: &'static str,
    ordinal: usize,
    mode: CausalFaultMode,
) -> Value {
    let (token, original_head, _) = fixture.start_and_queue();
    let _completion = fixture
        .state
        .finish_turn_ok_if_runtime_matches(&fixture.session, &token);
    // Nothing here may panic while StateInner is held; results are checked
    // after the guard is released.
    let snapshot = {
        let mut inner = fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session);
        index.and_then(|index| inner.session_mut_by_index(index)).map(|record| {
            let retained = record.queued_prompts.front().map(|queued| {
                (queued.pending_prompt.id.clone(), queued.engram_evaluate.is_some())
            });
            let recovered = match PersistedSessionRecord::from_record(record).into_record() {
                Ok(restored) => {
                    *record = restored;
                    Ok(record.engram.recovered_admission)
                }
                Err(error) => Err(format!("{error:#}")),
            };
            (retained, recovered)
        })
    };
    let (retained, recovered) = snapshot.expect("the fixture session exists");
    let recovered = recovered.expect("the saved record loads");
    assert_eq!(
        retained,
        Some((original_head["id"].as_str().unwrap().to_owned(), true)),
        "the exact head keeps its prepared evaluate"
    );
    assert!(recovered, "the loader marks a retained admission as recovered");
    *fixture.transport.lost_request.lock().unwrap() = None;
    *fixture.transport.fault.lock().unwrap() = Some((operation, ordinal));
    *fixture.transport.mode.lock().unwrap() = mode;
    original_head
}

#[test]
fn root_causal_cold_restore_status_validation_keeps_its_request() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_evaluate", 2)));
    let operation = "session_status";
    cold_restored(&fixture, operation, 1, CausalFaultMode::MalformedRequestSecret);
    // Through the queue's own admission, its fallback and the committed card.
    let _resume = fixture.state.resume_session_queue(&fixture.session);
    let request = lost_request(&fixture);
    let committed = committed(&fixture);
    assert_eq!(request["operation"], operation);
    assert!(fixture.receiver.try_recv().is_err(), "no provider handoff");
    assert_absent(
        &committed,
        &[
            "opaque-response-sentinel-73f9",
            request["routing_token"].as_str().unwrap(),
        ],
    );
    let cause = last_cause(&committed);
    assert_eq!(cause["operation"], operation);
    assert_eq!(cause["attemptId"], request["idempotency_key"]);
    assert_eq!(cause["failureClass"], "protocol");
    assert_eq!(cause["boundary"], "Engram control response validation");
    assert_eq!(cause["remoteApplication"], "unknown");
    assert!(cause["message"].as_str().unwrap().contains("kind=Data; field path=$"));
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

// The closing branch interrupts the head before its checkpoint, and the
// existing interrupted-head hold writes no card for this admission. The
// attribution is pinned on the restore's error and the admission fallback.
#[test]
fn root_causal_cold_restore_checkpoint_validation_keeps_its_request() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = CausalRoot::new(Some(("turn_evaluate", 2)));
    let original_head = cold_restored(
        &fixture,
        "turn_checkpoint",
        2,
        CausalFaultMode::RecoveryMalformedRequestSecret,
    );
    let (target, owner) = {
        let inner = fixture.state.inner.lock().unwrap();
        (
            AppState::engram_binding_target_for_session_shape_locked(&inner, &fixture.session, true),
            inner
                .find_session_index(&fixture.session)
                .and_then(|index| EngramQueuedAdmissionOwner::capture(&inner.sessions[index])),
        )
    };
    let mut target = target.unwrap().expect("the session has a binding target");
    let owner = owner.expect("the retained head has an owner");
    target.admission_started_at = Some(fixture.state.engram_budget_clock().now());
    let error = fixture
        .state
        .restore_queued_engram_target(&mut target, &owner)
        .unwrap_err();
    let request = lost_request(&fixture);
    let (head, interrupted) = read_record(&fixture, |record| {
        let head = record.queued_prompts.front();
        (
            head.map(|queued| serde_json::to_value(&queued.pending_prompt)),
            head.is_some_and(|queued| queued.engram_interrupted),
        )
    });
    let head = head.transpose().unwrap();
    assert_eq!(request["operation"], "turn_checkpoint");
    assert_eq!(head.as_ref(), Some(&original_head), "existing decision: exact head retained");
    assert!(interrupted, "existing decision: interrupted before the checkpoint");
    assert!(fixture.receiver.try_recv().is_err(), "no provider handoff");
    let cause = error.causal_failure.clone().expect("the exact request is attached");
    let fallback = EngramCausalFailure::error(&error, "admission_binding", None, &[]);
    assert_eq!(fallback, cause, "the admission fallback keeps the attributed cause");
    let cause = serde_json::to_value(cause).unwrap();
    let text = format!("{cause} {}", error.message);
    for value in ["opaque-response-sentinel-73f9", request["routing_token"].as_str().unwrap()] {
        assert!(!text.contains(value), "a received value reached the diagnostic");
    }
    assert_eq!(cause["operation"], "turn_checkpoint");
    assert_eq!(cause["attemptId"], request["idempotency_key"]);
    assert_eq!(cause["failureClass"], "protocol");
    assert_eq!(cause["boundary"], "Engram control response validation");
    assert_eq!(cause["remoteApplication"], "unknown");
    assert!(cause["message"].as_str().unwrap().contains("kind=Data; field path=$"));
}
