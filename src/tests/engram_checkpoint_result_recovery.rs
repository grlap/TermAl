use super::*;

#[path = "engram_checkpoint_result_effects.rs"]
mod effects;

#[path = "engram_checkpoint_result_recovery_progress.rs"]
mod recovery_progress;

#[path = "engram_checkpoint_moved_authority.rs"]
mod moved_authority;

fn lost_checkpoint(label: &str, replies: Vec<ScriptedEngramControlResponse>) -> (CheckedTurn, EngramCheckpointOperation) {
    let turn = CheckedTurn::start_with(label, true, None,
        [ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport("reply lost")))]
            .into_iter().chain(replies).collect());
    turn.run("original-check", SIZE_TEST, EngramCommandExit::Code(0), || {});
    turn.finish();
    let operation = turn.record(|record| record.engram.checkpoint_operation.clone().unwrap());
    assert!(operation.has_uncertain_checkpoint_history());
    assert!(operation.protection.authority_confirmed);
    // Model the same currently held work authority on each recovery read.
    turn.transport.work_bindings.lock().unwrap().extend(
        (0..8).map(|_| Ok(operation.original.work_binding.clone())));
    (turn, operation)
}

fn assert_recovered(turn: &CheckedTurn, original: &EngramCheckpointOperation) {
    let sent = checkpoints(turn);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "recovery must replay the entire original request, including report and key");
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let canonical = loaded.engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.original.id == original.original.id).unwrap();
    assert_eq!(serde_json::to_value(&canonical.original).unwrap(), serde_json::to_value(&original.original).unwrap());
    canonical.validate().unwrap();
    assert!(matches!(canonical.attempts[0].result, Some(EngramCheckpointAttemptResult::Unknown { .. })));
    assert!(matches!(canonical.attempts.last().unwrap().result, Some(EngramCheckpointAttemptResult::Accepted)));
    assert!(matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. }));
    assert_eq!(canonical.protection.phase, EngramSessionObligationPhase::Confirmed);
    assert!(loaded.engram_confirmed_containment_ids.contains(&original.original.id));
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_none());
    assert_eq!(turn.transport.requests().iter().filter(|request|
        matches!(request.request["operation"].as_str(), Some("session_bind" | "turn_evaluate" | "turn_begin"))).count(), 3);
}

#[test]
fn original_checkpoint_unknown_automatic_tick_recovers_exact_request_and_report() {
    let (turn, original) = lost_checkpoint("unknown-auto", vec![checkpoint_reply(CHECK_GRANT)]);
    let now = chrono::Utc::now();
    turn.state.engram_abort_retry_tick(now);
    assert_eq!(checkpoints(&turn).len(), 1, "the first tick schedules the existing delay");
    turn.state.engram_abort_retry_tick(now + chrono::Duration::seconds(3));
    assert_recovered(&turn, &original);
}

#[test]
fn original_checkpoint_busy_does_not_certify_older_unknown() {
    let (turn, original) = lost_checkpoint("unknown-busy", vec![
        remote_error_reply("control_store_busy"), checkpoint_reply(CHECK_GRANT)]);
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).is_err());
    let after = turn.record(|record| record.engram.checkpoint_operation.clone().unwrap());
    assert!(after.has_uncertain_checkpoint_history());
    assert!(matches!(after.attempts.last().unwrap().result,
        Some(EngramCheckpointAttemptResult::CertifiedNotApplied { .. })));
    assert!(certified_checkpoint_replay_target_locked(&turn.state.inner.lock().unwrap(), &turn.session_id, None).is_none());
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    let sent = checkpoints(&turn);
    assert_eq!(sent.len(), 3);
    assert!(sent.iter().all(|request| request == &sent[0]));
    assert!(turn.record(|record| record.engram.active_grant_id.is_none()));
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let canonical = loaded.engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.original.id == original.original.id).unwrap();
    assert_eq!(canonical.attempts.len(), 3);
    assert!(!canonical.has_uncertain_checkpoint_history(), "the definitive matching receipt resolves older uncertainty");
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_none());
}

#[test]
fn original_checkpoint_unknown_survives_whole_state_load_and_recovers_with_current_owner() {
    let (turn, original) = lost_checkpoint("unknown-restart", vec![checkpoint_reply(CHECK_GRANT)]);
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let clock = turn.state.engram_budget_clock();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        *inner = loaded;
        inner.engram_budget_clock = clock;
    }
    install_control_only_transport(&turn.state, turn.transport.clone());
    let now = chrono::Utc::now();
    turn.state.engram_abort_retry_tick(now);
    turn.state.engram_abort_retry_tick(now + chrono::Duration::seconds(3));
    let current = turn.record(|record| record.engram.checkpoint_operation.clone().unwrap());
    assert_eq!(current.attempts.last().unwrap().runtime_episode, "None");
    assert_ne!(current.attempts[0].runtime_episode, current.attempts.last().unwrap().runtime_episode);
    assert_eq!(current.attempts.last().unwrap().active_turn_generation, 0);
    assert_recovered(&turn, &original);
}

#[test]
fn original_checkpoint_orphan_stays_visible_until_canonical_recovery() {
    let (turn, original) = lost_checkpoint("unknown-orphan", vec![checkpoint_reply(CHECK_GRANT)]);
    turn.state.kill_session(&turn.session_id).unwrap();
    assert_eq!(checkpoints(&turn).len(), 1, "administrative removal is not another checkpoint trigger");
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.find_session_index(&turn.session_id).is_none());
    let summaries = engram_control_recovery_summaries(&loaded);
    let summary = summaries.iter().find(|summary| summary.session_id == turn.session_id).unwrap();
    assert!(summary.excluded);
    assert!(!summary.settlement_confirmed);
    assert!(summary.next_actions.is_empty(), "a deleted row has no reachable session action");
    assert_eq!(serde_json::to_value(&loaded.engram_session_obligations.checkpoint_operations[0]).unwrap(),
        serde_json::to_value(&original).unwrap());
    let now = chrono::Utc::now();
    turn.state.engram_abort_retry_tick(now);
    turn.state.engram_abort_retry_tick(now + chrono::Duration::seconds(3));
    assert_recovered(&turn, &original);
    assert!(engram_control_recovery_summaries(&turn.state.inner.lock().unwrap()).is_empty());
}

fn raw_session_rows(connection: &rusqlite::Connection) -> Vec<(String, String)> {
    connection.prepare("SELECT id,value_json FROM sessions ORDER BY id").unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
        .collect::<rusqlite::Result<Vec<_>>>().unwrap()
}

#[test]
fn original_checkpoint_reader_floor_preserves_raw_unknown_units_rows_and_generation() {
    let (turn, original) = lost_checkpoint("unknown-floor", vec![]);
    turn.state.shutdown_persist_blocking();
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    let units = read_metadata_units_unchecked(&connection).unwrap();
    let rows = raw_session_rows(&connection);
    let generation = metadata_unit_generation(&connection).unwrap();
    connection.execute("UPDATE meta SET value=?1 WHERE key='schema_version'",
        [SQLITE_CHECKPOINT_RESULT_PREDECESSOR]).unwrap();
    drop(connection);
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), SQLITE_SCHEMA_VERSION);
    assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), units);
    assert_eq!(raw_session_rows(&connection), rows);
    assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
    assert_eq!(loaded.metadata_predecessor.load(std::sync::atomic::Ordering::Acquire), generation);
    let canonical = loaded.engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.original.id == original.original.id).unwrap();
    assert_eq!(serde_json::to_value(canonical).unwrap(), serde_json::to_value(original).unwrap());
}

#[test]
fn original_checkpoint_refused_result_requires_successor_floor_and_keeps_older_unknown() {
    let (turn, original) = lost_checkpoint("refused-floor", vec![checkpoint_refusal_reply("evidence_rejected")]);
    assert!(!turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    let operation = turn.record(|record| record.engram.checkpoint_operation.clone().unwrap());
    assert!(operation.has_uncertain_checkpoint_history(), "a later invocation refusal is not settlement of the older Unknown");
    assert!(matches!(operation.attempts.last().unwrap().result, Some(EngramCheckpointAttemptResult::Refused { .. })));
    assert_ne!(operation.protection.phase, EngramSessionObligationPhase::Confirmed);
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_some());
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection.execute("UPDATE meta SET value=?1 WHERE key='schema_version'",
        [SQLITE_CHECKPOINT_RESULT_PREDECESSOR]).unwrap();
    let units = read_metadata_units_unchecked(&connection).unwrap();
    let rows = raw_session_rows(&connection);
    let generation = metadata_unit_generation(&connection).unwrap();
    drop(connection);
    let error = match load_state(turn.state.persistence_path.as_path()) {
        Ok(_) => panic!("new Refused under the old header must reject, not normalize"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("checkpoint Refused result under predecessor floor"));
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), SQLITE_CHECKPOINT_RESULT_PREDECESSOR);
    assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), units);
    assert_eq!(raw_session_rows(&connection), rows);
    assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
}

#[test]
fn original_checkpoint_refusal_retains_only_the_validated_code() {
    let (turn, original) = lost_checkpoint("refused-private-fields", vec![
        ScriptedEngramControlResponse::Reply(Ok(json!({
            "decision": "refuse", "code": "evidence_rejected",
            "private_detail": "fixture credential and private report text",
            "extension": { "payload": "not durable result evidence" }
        })))
    ]);
    assert!(!turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let operation = loaded.engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.original.id == original.original.id).unwrap();
    let Some(EngramCheckpointAttemptResult::Refused { response }) =
        &operation.attempts.last().unwrap().result else { panic!("expected refusal") };
    assert_eq!(response, &json!({ "decision": "refuse", "code": "evidence_rejected" }));
    operation.validate().unwrap();
    let mut unsupported = operation.clone();
    let Some(EngramCheckpointAttemptResult::Refused { response }) =
        &mut unsupported.attempts.last_mut().unwrap().result else { unreachable!() };
    response["private_detail"] = json!("must be rejected on load");
    assert!(unsupported.validate().is_err());
    assert!(checkpoint_refusal_projection(&"x".repeat(65)).is_err());
    assert!(checkpoint_refusal_projection("private payload with spaces").is_err());
    assert!(operation.has_uncertain_checkpoint_history());
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_some());
}

#[path = "engram_checkpoint_result_recovery_boundaries.rs"]
mod recovery_boundaries;
