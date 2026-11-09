// Historical attempt lineage/capacity and ordinary progress alongside data.
// Automatic replay cadence, retry slots and boot recovery no longer exist.
use super::*;

#[test]
fn historical_refusal_preserves_every_earlier_unknown_and_certificate_through_reload() {
    let (_root, path, mut inner, session, mut operation) = passive_state("checkpoint-lineage-data");
    for code in ["validation_failed", "invalid_routing_token"] {
        append_historical_result(
            &mut operation,
            EngramCheckpointAttemptResult::Unknown {
                code: Some(code.into()),
            },
        );
    }
    append_historical_result(
        &mut operation,
        EngramCheckpointAttemptResult::CertifiedNotApplied {
            code: "control_request_expired".into(),
        },
    );
    let prefix = serde_json::to_value(&operation.attempts).unwrap();
    let original = serde_json::to_value(&operation.original).unwrap();
    append_historical_result(
        &mut operation,
        EngramCheckpointAttemptResult::Refused {
            response: json!({"decision":"refuse","code":"evidence_rejected"}),
        },
    );
    install_historical_checkpoint(&mut inner, &session, &operation);
    persist_state(&path, &inner).unwrap();
    let loaded = load_state(&path).unwrap().unwrap();
    assert_historical_operation(&loaded, &operation);
    assert_eq!(serde_json::to_value(&operation.original).unwrap(), original);
    assert_eq!(
        serde_json::to_value(&operation.attempts[..4]).unwrap(),
        prefix
    );
    assert_ne!(
        operation.protection.phase,
        EngramSessionObligationPhase::Confirmed
    );
    assert!(operation.protection.settlement_evidence.is_null());
}

#[test]
fn historical_attempt_inventory_rejects_missing_over_capacity_and_broken_lineage() {
    let root = TestTempRoot::create("checkpoint-attempt-bounds");
    let operation = passive_checkpoint(root.path(), "historical-session");
    let mut missing = operation.clone();
    missing.attempts.clear();
    assert!(missing.validate().is_err());
    let mut oversized = operation.clone();
    oversized.attempts =
        vec![operation.attempts[0].clone(); ENGRAM_SESSION_OBLIGATION_MAX_ENTRIES + 1];
    assert!(
        oversized.validate().is_err(),
        "capacity is checked before lineage traversal"
    );
    for changed in ["predecessor", "identity", "runtime", "paired-head"] {
        let mut invalid = operation.clone();
        append_historical_result(
            &mut invalid,
            EngramCheckpointAttemptResult::Unknown { code: None },
        );
        match changed {
            "predecessor" => invalid.attempts[1].predecessor = None,
            "identity" => invalid.attempts[1].id = invalid.attempts[0].id.clone(),
            "runtime" => invalid.attempts[1].runtime_episode.clear(),
            "paired-head" => invalid.attempts[1].queued_prompt_id = Some("unpaired-head".into()),
            _ => unreachable!(),
        }
        assert!(invalid.validate().is_err(), "{changed}");
    }
}

#[test]
fn repeated_passive_load_does_not_append_attempts_or_advance_metadata_generation() {
    let (_root, path, _inner, _session, operation) = passive_state("checkpoint-readonly-history");
    let connection = rusqlite::Connection::open(&path).unwrap();
    let units = read_metadata_units_unchecked(&connection).unwrap();
    let rows = raw_session_rows(&connection);
    let generation = metadata_unit_generation(&connection).unwrap();
    drop(connection);
    for _ in 0..3 {
        let loaded = load_state(&path).unwrap().unwrap();
        assert_historical_operation(&loaded, &operation);
        assert_eq!(
            loaded.engram_session_obligations.checkpoint_operations[0]
                .attempts
                .len(),
            1
        );
    }
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), units);
    assert_eq!(raw_session_rows(&connection), rows);
    assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
}

#[test]
fn ordinary_root_progresses_beside_historical_unknown_without_control_replay() {
    let turn = EOCheckedTurn::start("ordinary-beside-unknown-history", true);
    let operation = passive_checkpoint(&turn.root, &turn.session_id);
    let original = serde_json::to_value(&operation).unwrap();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        install_historical_checkpoint(&mut inner, &turn.session_id, &operation);
        turn.state.commit_locked(&mut inner).unwrap();
    }
    turn.run(
        "ordinary-check",
        SIZE_TEST,
        EngramCommandExit::Code(0),
        || {},
    );
    let report = turn.finish();
    assert_eq!(
        report["occurrence"]["observed_checks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_ordinary_observation_has_no_native_credit(&turn, &report);
    turn.record(|record| {
        assert_eq!(
            record.engram.historical_control["engramCheckpointOperation"],
            original
        );
        assert!(
            !record
                .engram
                .historical_control
                .contains_key("engramOwedBinds")
        );
    });
    let inner = turn.state.inner.lock().unwrap();
    assert_eq!(
        serde_json::to_value(&inner.engram_session_obligations.checkpoint_operations[0]).unwrap(),
        original
    );
}
