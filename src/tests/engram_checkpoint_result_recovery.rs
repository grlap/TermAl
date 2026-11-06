// Historical checkpoint storage integrity. These are stored-data specimens,
// never replies to a live RPC and never sources of verification credit.
use super::*;

#[path = "engram_checkpoint_result_effects.rs"]
mod effects;

#[path = "engram_checkpoint_result_recovery_progress.rs"]
mod recovery_progress;

pub(in super::super) fn passive_checkpoint(root: &FsPath, session: &str) -> EngramCheckpointOperation {
    let connection = EngramConnectionConfig {
        binary_path: PathBuf::from("engram"), project_file: root.join(".engram-project"),
        home: root.to_path_buf(), project_root: root.to_path_buf(),
        actor_id: "historical-actor".into(), actor_context: None, session_id: session.into(),
    };
    let authority = EngramAuthorityStoreKey {
        database_path: root.join("historical-engram.sqlite"), project_id: "historical-project".into(),
    };
    let request = json!({"operation":"turn_checkpoint", "routing_token":"historical-token",
        "grant_id":"historical-grant", "next_intent":"wait", "idempotency_key":"historical-key",
        "observations":[{"original_report":"retained verbatim", "unknown_nested":[1,null,{"x":true}]}]});
    let original = json!({"id":"historical-checkpoint", "authority":"historical-authority",
        "connection":connection, "settings":{"authorityStoreKey":authority}, "workBinding":null,
        "request":request, "dispatchGeneration":0, "activeTurnGeneration":0,
        "runtimeEpisode":"historical-episode", "checkpointOwnerGeneration":null});
    serde_json::from_value(json!({"original":original, "progress":{"state":"possibly_sent"},
        "protection":{"id":"historical-checkpoint", "projectId":"historical-local-project",
            "authority":authority, "authorityConfirmed":true, "connection":connection,
            "externalRef":format!("termal:session:{session}"), "localSessionId":session,
            "ownerGeneration":0, "promptId":null, "routingToken":"historical-token",
            "grantId":"historical-grant", "kind":"existing_checkpoint", "phase":"sent",
            "basis":"existing_checkpoint", "reasonCode":"original_checkpoint_unknown",
            "frozenRequest":original, "settlementEvidence":null},
        "attempts":[{"id":"historical-checkpoint", "predecessor":null,
            "dispatchGeneration":0, "activeTurnGeneration":0, "runtimeEpisode":"historical-episode",
            "queuedPromptId":null, "queuedFingerprint":null,
            "result":{"result":"unknown", "code":"transport_error"}}]})).unwrap()
}

pub(in super::super) fn append_historical_result(operation: &mut EngramCheckpointOperation, result: EngramCheckpointAttemptResult) {
    let mut attempt = operation.attempts[0].clone();
    attempt.id = format!("historical-attempt-{}", operation.attempts.len());
    attempt.predecessor = operation.attempts.last().map(|previous| previous.id.clone());
    attempt.result = Some(result);
    operation.attempts.push(attempt);
}

pub(in super::super) fn accepted_history(operation: &mut EngramCheckpointOperation) {
    let response = json!({"decision":"checkpointed", "receipt":{
        "grant_id":operation.protection.grant_id, "cursor":7, "confirmed_cursor":6,
        "checkpoint":"historical-checkpoint-record", "verification_evidence":["historical-evidence-record"]}});
    append_historical_result(operation, EngramCheckpointAttemptResult::Accepted);
    operation.progress = EngramExistingCheckpointProgress::Accepted { response: response.clone() };
    operation.protection.phase = EngramSessionObligationPhase::Confirmed;
    operation.protection.settlement_evidence = response;
    operation.validate().unwrap();
}

pub(super) fn passive_state(label: &str) -> (TestTempRoot, PathBuf, StateInner, String, EngramCheckpointOperation) {
    let root = TestTempRoot::create(label);
    let mut inner = StateInner::new();
    let session = inner.create_session(Agent::Codex, None,
        root.path().to_string_lossy().into_owned(), None, None).session.id;
    let operation = passive_checkpoint(root.path(), &session);
    install_historical_checkpoint(&mut inner, &session, &operation);
    let path = root.path().join("historical-state.sqlite");
    persist_state(&path, &inner).unwrap();
    (root, path, inner, session, operation)
}

pub(in super::super) fn install_historical_checkpoint(inner: &mut StateInner, session: &str, operation: &EngramCheckpointOperation) {
    inner.engram_session_obligations.checkpoint_operations = vec![operation.clone()];
    inner.engram_session_obligations.validate().unwrap();
    inner.session_mut(session).unwrap().engram.historical_control
        .insert("engramCheckpointOperation".into(), serde_json::to_value(operation).unwrap());
}

pub(super) fn raw_session_rows(connection: &rusqlite::Connection) -> Vec<(String, String)> {
    connection.prepare("SELECT id,value_json FROM sessions ORDER BY id").unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
        .collect::<rusqlite::Result<Vec<_>>>().unwrap()
}

pub(super) fn assert_historical_operation(inner: &StateInner, operation: &EngramCheckpointOperation) {
    let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
        .find(|item| item.original.id == operation.original.id).unwrap();
    assert_eq!(serde_json::to_value(canonical).unwrap(), serde_json::to_value(operation).unwrap());
    canonical.validate().unwrap();
    assert!(inner.test_run_observations.is_empty(), "loading historical receipts cannot invent native verification");
}

#[test]
fn historical_checkpoint_keeps_the_entire_original_request_and_unknown_attempt() {
    let root = TestTempRoot::create("checkpoint-original-data");
    let original = passive_checkpoint(root.path(), "historical-session");
    let raw = serde_json::to_value(&original).unwrap();
    let decoded: EngramCheckpointOperation = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), raw);
    assert_eq!(raw["original"]["request"]["observations"][0]["unknown_nested"], json!([1,null,{"x":true}]));
    assert!(matches!(original.attempts[0].result, Some(EngramCheckpointAttemptResult::Unknown { .. })));
}

#[test]
fn historical_busy_certificate_does_not_replace_an_earlier_unknown() {
    let root = TestTempRoot::create("checkpoint-local-certificate");
    let mut operation = passive_checkpoint(root.path(), "historical-session");
    let original = serde_json::to_value(&operation.original).unwrap();
    let first = serde_json::to_value(&operation.attempts[0]).unwrap();
    append_historical_result(&mut operation, EngramCheckpointAttemptResult::CertifiedNotApplied { code: "control_store_busy".into() });
    operation.validate().unwrap();
    assert_eq!(serde_json::to_value(&operation.original).unwrap(), original);
    assert_eq!(serde_json::to_value(&operation.attempts[0]).unwrap(), first);
    assert_ne!(operation.protection.phase, EngramSessionObligationPhase::Confirmed);
    assert!(operation.protection.settlement_evidence.is_null());
}

#[test]
fn historical_checkpoint_survives_whole_state_load_as_passive_data() {
    let (_root, path, _inner, session, operation) = passive_state("checkpoint-passive-load");
    let loaded = load_state(&path).unwrap().unwrap();
    assert_historical_operation(&loaded, &operation);
    let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
    assert_eq!(record.engram.historical_control["engramCheckpointOperation"], serde_json::to_value(&operation).unwrap());
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.engram.historical_control.contains_key("engramOwedBinds"));
}

#[test]
fn removing_a_stopped_session_preserves_its_unknown_original_without_replay_or_resume() {
    let turn = super::super::uncertain_begin::OrdinaryRoot::new("remove-stopped-passive-original");
    // Only a running session can be stopped: start an ordinary turn first.
    deliver_turn_dispatch(&turn.state, turn.prepare("Running ordinary turn.")).unwrap();
    turn.prompt();
    turn.state.stop_session(&turn.session).expect("stop the ordinary running session first");
    let head = turn.enqueue("retained original content\nżółw 🐢", QueuedPromptSource::User);
    turn.edit(|record| {
        record.queued_prompts[0].promotion_disposition_known = false;
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts[0].engram_evaluate = Some(json!({
            "request":{"operation":"turn_evaluate", "idempotency_key":"historical-evaluate-key"},
            "outcome":"unknown", "original":{"head":head,"future":[null,"unchanged"]}
        }));
    });
    let original_evaluate = turn.record(|record| record.queued_prompts[0].engram_evaluate.clone());
    let operation = passive_checkpoint(&turn.root, &turn.session);
    let original = serde_json::to_value(&operation).unwrap();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        install_historical_checkpoint(&mut inner, &turn.session, &operation);
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let saved_before = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let record = &saved_before.sessions[saved_before.find_session_index(&turn.session).unwrap()];
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.queued_prompts[0].pending_prompt.id, head);
    assert_eq!(record.queued_prompts[0].pending_prompt.text, "retained original content\nżółw 🐢");
    // Complete transcript residency normalizes the legacy promotion bit;
    // the retained handoff still proves this row must never run again.
    assert!(record.queued_prompts[0].promotion_disposition_known);
    assert!(record.queued_prompts[0].engram_ordinary_handoff);
    assert!(!ordinary_queue_entry_is_runnable(record, &record.queued_prompts[0]));
    assert_eq!(record.queued_prompts[0].engram_evaluate, original_evaluate);
    assert_historical_operation(&saved_before, &operation);

    turn.state.kill_session(&turn.session)
        .expect("passive unconfirmed control history cannot veto explicit removal");
    assert!(turn.state.inner.lock().unwrap().find_session_index(&turn.session).is_none());
    turn.state.shutdown_persist_blocking();
    let saved_after = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(saved_after.find_session_index(&turn.session).is_none());
    assert_historical_operation(&saved_after, &operation);
    assert_eq!(serde_json::to_value(&saved_after.engram_session_obligations.checkpoint_operations[0]).unwrap(), original);
    assert_eq!(saved_after.engram_session_obligations.checkpoint_operations[0].attempts.len(), 1);
    assert!(matches!(saved_after.engram_session_obligations.checkpoint_operations[0].progress,
        EngramExistingCheckpointProgress::PossiblySent));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn orphan_historical_checkpoint_survives_without_a_live_session() {
    let (root, _path, _inner, _session, operation) = passive_state("checkpoint-passive-orphan");
    let mut orphan = StateInner::new();
    orphan.engram_session_obligations.checkpoint_operations.push(operation.clone());
    let path = root.path().join("orphan.sqlite");
    persist_state(&path, &orphan).unwrap();
    let loaded = load_state(&path).unwrap().unwrap();
    assert!(loaded.sessions.is_empty());
    assert_historical_operation(&loaded, &operation);
}

#[test]
fn original_checkpoint_reader_floor_preserves_raw_unknown_units_rows_with_native_extension() {
    let (_root, path, _inner, _session, original) = passive_state("checkpoint-unknown-floor");
    let connection = rusqlite::Connection::open(&path).unwrap();
    let units = read_metadata_units_unchecked(&connection).unwrap();
    let rows = raw_session_rows(&connection);
    let generation = metadata_unit_generation(&connection).unwrap();
    connection.execute("DELETE FROM state_metadata_units WHERE field_name='remoteTestRunFacts'", []).unwrap();
    connection.execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [SQLITE_CHECKPOINT_RESULT_PREDECESSOR]).unwrap();
    drop(connection);
    let loaded = load_state(&path).unwrap().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), SQLITE_SCHEMA_VERSION);
    assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), units);
    assert_eq!(raw_session_rows(&connection), rows);
    // The vocabulary-only step preserves the units; the following native
    // completion descriptor extension advances the generation exactly once.
    assert_eq!(metadata_unit_generation(&connection).unwrap(), generation + 1);
    assert_eq!(loaded.metadata_predecessor.load(std::sync::atomic::Ordering::Acquire), generation + 1);
    assert_historical_operation(&loaded, &original);
}

#[test]
fn original_checkpoint_refused_result_requires_successor_floor_and_keeps_older_unknown() {
    let (_root, path, mut inner, session, mut operation) = passive_state("checkpoint-refused-floor");
    let first = serde_json::to_value(&operation.attempts[0]).unwrap();
    append_historical_result(&mut operation, EngramCheckpointAttemptResult::Refused {
        response: json!({"decision":"refuse","code":"evidence_rejected"}) });
    install_historical_checkpoint(&mut inner, &session, &operation);
    persist_state(&path, &inner).unwrap();
    let loaded = load_state(&path).unwrap().unwrap();
    assert_historical_operation(&loaded, &operation);
    assert_eq!(serde_json::to_value(&operation.attempts[0]).unwrap(), first);
    assert_ne!(operation.protection.phase, EngramSessionObligationPhase::Confirmed);
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute("DELETE FROM state_metadata_units WHERE field_name='remoteTestRunFacts'", []).unwrap();
    connection.execute("UPDATE meta SET value=?1 WHERE key='schema_version'", [SQLITE_CHECKPOINT_RESULT_PREDECESSOR]).unwrap();
    let units = read_metadata_units_unchecked(&connection).unwrap();
    let rows = raw_session_rows(&connection);
    let generation = metadata_unit_generation(&connection).unwrap();
    drop(connection);
    let error = match load_state(&path) { Ok(_) => panic!("Refused under old header must reject"), Err(error) => error };
    assert!(format!("{error:#}").contains("checkpoint Refused result under predecessor floor"));
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(required_state_meta_value(&connection, "schema_version").unwrap(), SQLITE_CHECKPOINT_RESULT_PREDECESSOR);
    assert_eq!(read_metadata_units_unchecked(&connection).unwrap(), units);
    assert_eq!(raw_session_rows(&connection), rows);
    assert_eq!(metadata_unit_generation(&connection).unwrap(), generation);
}

#[test]
fn original_checkpoint_refusal_retains_only_the_validated_code() {
    let root = TestTempRoot::create("checkpoint-refusal-code");
    let mut operation = passive_checkpoint(root.path(), "historical-session");
    append_historical_result(&mut operation, EngramCheckpointAttemptResult::Refused {
        response: json!({"decision":"refuse","code":"evidence_rejected"}) });
    operation.validate().unwrap();
    let raw = serde_json::to_value(&operation).unwrap();
    assert_eq!(raw["attempts"][1]["result"]["response"], json!({"decision":"refuse","code":"evidence_rejected"}));
    for code in ["x".repeat(65), "private payload with spaces".into()] {
        let mut unsupported = operation.clone();
        let Some(EngramCheckpointAttemptResult::Refused { response }) = &mut unsupported.attempts[1].result else { unreachable!() };
        response["code"] = json!(code);
        assert!(unsupported.validate().is_err());
    }
    let mut unsupported = raw;
    unsupported["attempts"][1]["result"]["response"]["private_detail"] = json!("must not enter the stored projection");
    assert!(serde_json::from_value::<EngramCheckpointOperation>(unsupported).is_err());
    assert!(matches!(operation.attempts[0].result, Some(EngramCheckpointAttemptResult::Unknown { .. })));
    assert!(operation.protection.settlement_evidence.is_null());
}

#[path = "engram_checkpoint_result_recovery_boundaries.rs"]
mod recovery_boundaries;
