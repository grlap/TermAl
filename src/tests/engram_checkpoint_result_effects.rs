// Actual SQLite cuts for passive original/report/batch data. Historical
// receipts never create verification records or convert pending batches.
use super::*;

fn pending_batches() -> Value {
    json!([{"checkpointGrantId":"historical-grant","producerObservationId":"original-observation",
        "measurement":{"workspaceId":"saved-workspace","sourceRevision":"saved-source","measuredAt":"2026-10-01T01:02:03Z"},
        "targets":[{"workId":"original-member","claimId":"original-claim","claimFence":7,"rootGeneration":3}],
        "pendedAt":"2026-10-01T01:02:03Z","unknownNested":{"exact":[true,null,"żółw"]}},
        {"checkpointGrantId":"successor-grant","targets":[{"workId":"successor-member"}]}])
}

// Original persisted wire, including fields unknown to the current host.
// This claims neither a registered native record nor a current send duty.
fn historical_owed_bind() -> Value {
    json!([{
        "original": "historical-verification-reference",
        "measurement": {"workspace_id":"original-workspace", "source_revision":"original-source",
            "measured_at":"2026-10-01T01:02:03Z"},
        "target": {"workId":"independent-member", "shortRef":"member", "claimId":"original-claim",
            "claimFence":7, "rootGeneration":3},
        "attempts":1, "firstOwedAt":"2026-10-01T01:02:03Z",
        "lastDetail":"earlier answer unknown", "nextAttemptAt":"2026-10-01T01:12:03Z",
        "future":{"unknown":[null,"żółw"]}
    }])
}

#[test]
fn historical_unknown_then_refused_keeps_exact_batch_inputs_and_original_through_reload() {
    let (_root, path, mut inner, session, mut operation) = passive_state("batch-history-refused");
    let expected = pending_batches();
    let original = serde_json::to_value(&operation.original).unwrap();
    let first = serde_json::to_value(&operation.attempts[0]).unwrap();
    append_historical_result(
        &mut operation,
        EngramCheckpointAttemptResult::Refused {
            response: json!({"decision":"refuse","code":"stale_fence"}),
        },
    );
    install_historical_checkpoint(&mut inner, &session, &operation);
    inner
        .session_mut(&session)
        .unwrap()
        .engram
        .historical_control
        .insert("engramPendingBatches".into(), expected.clone());
    persist_state(&path, &inner).unwrap();
    let loaded = load_state(&path).unwrap().unwrap();
    assert_historical_operation(&loaded, &operation);
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert_eq!(
        record.engram.historical_control["engramPendingBatches"],
        expected
    );
    assert!(
        !record
            .engram
            .historical_control
            .contains_key("engramOwedBinds")
    );
    assert_eq!(serde_json::to_value(&operation.original).unwrap(), original);
    assert_eq!(serde_json::to_value(&operation.attempts[0]).unwrap(), first);
    assert_ne!(
        operation.protection.phase,
        EngramSessionObligationPhase::Confirmed
    );
}

#[test]
fn historical_accepted_receipt_does_not_convert_batches_or_invent_native_credit() {
    let (_root, path, mut inner, session, mut operation) = passive_state("batch-history-accepted");
    let expected = pending_batches();
    accepted_history(&mut operation);
    install_historical_checkpoint(&mut inner, &session, &operation);
    inner
        .session_mut(&session)
        .unwrap()
        .engram
        .historical_control
        .insert("engramPendingBatches".into(), expected.clone());
    persist_state(&path, &inner).unwrap();
    for _ in 0..2 {
        let loaded = load_state(&path).unwrap().unwrap();
        assert_historical_operation(&loaded, &operation);
        let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
        assert_eq!(
            record.engram.historical_control["engramPendingBatches"],
            expected
        );
        assert!(
            !record
                .engram
                .historical_control
                .contains_key("engramOwedBinds")
        );
        assert!(loaded.test_run_observations.is_empty());
    }
}

#[test]
fn historical_accepted_data_and_raw_batches_share_an_atomic_sqlite_cut() {
    let (_root, path, mut inner, session, mut operation) =
        passive_state("batch-history-transaction");
    let expected = pending_batches();
    inner
        .session_mut(&session)
        .unwrap()
        .engram
        .historical_control
        .insert("engramPendingBatches".into(), expected.clone());
    persist_state(&path, &inner).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let units = read_metadata_units_unchecked(&connection).unwrap();
    let rows = raw_session_rows(&connection);
    let generation = metadata_unit_generation(&connection).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_historical_cut BEFORE UPDATE ON state_metadata_units
        WHEN NEW.field_name='engramSessionObligations'
        BEGIN SELECT RAISE(ABORT,'historical atomic cut fault'); END;",
        )
        .unwrap();
    drop(connection);
    accepted_history(&mut operation);
    install_historical_checkpoint(&mut inner, &session, &operation);
    let error = persist_state(&path, &inner)
        .expect_err("the real writer must reject the injected atomic cut");
    assert!(format!("{error:#}").contains("historical atomic cut fault"));
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), units);
    assert_eq!(raw_session_rows(&connection), rows);
    assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
    assert!(connection.is_autocommit());
    connection
        .execute_batch("DROP TRIGGER fail_historical_cut")
        .unwrap();
    drop(connection);
    let old_cut = load_state(&path).unwrap().unwrap();
    assert!(matches!(
        old_cut.engram_session_obligations.checkpoint_operations[0].progress,
        EngramExistingCheckpointProgress::PossiblySent
    ));
    persist_state(&path, &inner).unwrap();
    let new_cut = load_state(&path).unwrap().unwrap();
    assert_historical_operation(&new_cut, &operation);
    let record = &new_cut.sessions[new_cut.find_session_index(&session).unwrap()];
    assert_eq!(
        record.engram.historical_control["engramPendingBatches"],
        expected
    );
    assert!(
        !record
            .engram
            .historical_control
            .contains_key("engramOwedBinds")
    );
}

#[test]
fn historical_owed_identity_survives_passive_checkpoint_data_roundtrip() {
    let (_root, path, mut inner, session, mut operation) =
        passive_state("batch-independent-native");
    let owed = historical_owed_bind();
    let pending = pending_batches();
    inner
        .session_mut(&session)
        .unwrap()
        .engram
        .historical_control
        .insert("engramOwedBinds".into(), owed.clone());
    inner
        .session_mut(&session)
        .unwrap()
        .engram
        .historical_control
        .insert("engramPendingBatches".into(), pending.clone());
    accepted_history(&mut operation);
    install_historical_checkpoint(&mut inner, &session, &operation);
    persist_state(&path, &inner).unwrap();
    let loaded = load_state(&path).unwrap().unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert_eq!(record.engram.historical_control["engramOwedBinds"], owed);
    assert_eq!(
        record.engram.historical_control["engramPendingBatches"],
        pending
    );
    assert!(loaded.test_run_observations.is_empty());
}

#[test]
fn ordinary_observability_toggle_preserves_historical_batches_and_owed_data() {
    let turn = EOCheckedTurn::start("batch-history-ordinary-toggle", true);
    let pending = pending_batches();
    let owed = historical_owed_bind();
    turn.record_mut(|record| {
        record
            .engram
            .historical_control
            .insert("engramPendingBatches".into(), pending.clone());
        record
            .engram
            .historical_control
            .insert("engramOwedBinds".into(), owed.clone());
    });
    let project = turn.record(|record| record.session.project_id.clone().unwrap());
    let mut settings = turn
        .state
        .inner
        .lock()
        .unwrap()
        .find_project(&project)
        .unwrap()
        .engram
        .clone()
        .unwrap();
    settings.execution_observability = false;
    turn.state
        .update_project_engram_settings(&project, settings)
        .unwrap();
    turn.record(|record| {
        assert_eq!(
            record.engram.historical_control["engramPendingBatches"],
            pending
        );
        assert_eq!(record.engram.historical_control["engramOwedBinds"], owed);
        assert_eq!(record.session.status, SessionStatus::Active);
    });
    let request = turn.finish();
    assert!(request.get("verification_evidence").is_none());
    for sent in turn.transport.requests() {
        assert!(!matches!(
            sent.request["operation"].as_str(),
            Some(
                "session_bind"
                    | "session_status"
                    | "turn_grant_read"
                    | "turn_evaluate"
                    | "turn_begin"
                    | "turn_checkpoint"
            )
        ));
    }
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let record = &loaded.sessions[loaded.find_session_index(&turn.session_id).unwrap()];
    assert_eq!(
        record.engram.historical_control["engramPendingBatches"],
        pending
    );
    assert_eq!(record.engram.historical_control["engramOwedBinds"], owed);
}
