//! Original-result recovery through the real pinned producer and disposable stores.
//! Provider delivery is simulated; checkpoint persistence and process replacement are real.
use super::*;

const CHECKPOINT_PRODUCER_SHA256: &str =
    "e02672e49f048522e931239a61548c8e113e57a25125c105073df2ec0e38e490";

struct LostOriginalCheckpointTransport {
    inner: Arc<BoundaryFaultTransport>,
    commit_before_loss: Mutex<Option<bool>>,
    checkpoint_requests: Mutex<Vec<Value>>,
    discarded_reply: Mutex<Option<Value>>,
    busy_database: Mutex<Option<PathBuf>>,
}

impl LostOriginalCheckpointTransport {
    fn new(inner: Arc<BoundaryFaultTransport>, loss: Option<bool>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            commit_before_loss: Mutex::new(loss),
            checkpoint_requests: Mutex::new(Vec::new()),
            discarded_reply: Mutex::new(None),
            busy_database: Mutex::new(None),
        })
    }
}

impl EngramControlTransport for LostOriginalCheckpointTransport {
    fn request(&self, connection: &EngramConnectionConfig, request: &EngramControlRequest,
        timeout: Duration) -> std::result::Result<Value, EngramTransportError> {
        if !matches!(request, EngramControlRequest::TurnCheckpoint { .. }) {
            return self.inner.request(connection, request, timeout);
        }
        self.checkpoint_requests.lock().unwrap().push(serde_json::to_value(request).unwrap());
        let loss = self.commit_before_loss.lock().unwrap().take();
        if loss == Some(false) {
            self.inner.shutdown_session(&connection.session_id);
            return Err(EngramTransportError::transport("original checkpoint lost before producer write"));
        }
        let busy_database = self.busy_database.lock().unwrap().take();
        if let Some(database) = busy_database {
            let started = std::time::Instant::now();
            let EngramControlRequest::TurnCheckpoint { routing_token, .. } = request else { unreachable!() };
            // Open the replacement process before taking the disposable writer;
            // the contention then occurs on checkpoint, before its result lookup.
            self.inner.request(connection, &EngramControlRequest::SessionStatus {
                routing_token: routing_token.clone(),
            }, timeout)?;
            let mut database = rusqlite::Connection::open(database).unwrap();
            let writer = database.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
            let result = self.inner.request(connection, request, timeout.saturating_sub(started.elapsed()));
            drop(writer);
            return result;
        }
        let reply = self.inner.request(connection, request, timeout)?;
        if loss == Some(true) {
            assert!(matches!(parse_engram_result::<EngramTurnCheckpointResponse>(reply.clone()).unwrap(),
                EngramTurnCheckpointResponse::Checkpointed { .. }));
            *self.discarded_reply.lock().unwrap() = Some(reply);
            self.inner.shutdown_session(&connection.session_id);
            return Err(EngramTransportError::transport("original committed checkpoint reply discarded"));
        }
        Ok(reply)
    }

    fn read_observation_policy(&self, connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey, timeout: Duration, host_workdir: &FsPath)
        -> std::result::Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.inner.read_observation_policy(connection, store, timeout, host_workdir)
    }

    fn read_work_binding(&self, connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>, timeout: Duration)
        -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner.read_work_binding(connection, preference, timeout)
    }

    fn read_work_binding_for_boot(&self, connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>, timeout: Duration)
        -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner.read_work_binding_for_boot(connection, preference, timeout)
    }

    fn read_held_claims(&self, connection: &EngramConnectionConfig, timeout: Duration)
        -> std::result::Result<EngramHeldClaims, EngramTransportError> {
        self.inner.read_held_claims(connection, timeout)
    }

    fn shutdown_session(&self, session_id: &str) { self.inner.shutdown_session(session_id); }
}

fn producer_checkpoint_row(database: &FsPath, original: &EngramCheckpointOperation)
    -> Option<(String, Vec<u8>, Vec<u8>)> {
    use rusqlite::OptionalExtension;
    let connection = rusqlite::Connection::open_with_flags(database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let EngramControlRequest::TurnCheckpoint { idempotency_key, .. } = &original.original.request
        else { panic!("original checkpoint request") };
    connection.query_row("SELECT intent_hash,intent_json,result_json FROM control_operation_results
        WHERE session_id=?1 AND operation='turn_checkpoint' AND idempotency_key=?2",
        rusqlite::params![original.original.connection.session_id, idempotency_key],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional().unwrap()
}

fn producer_connection_token(database: &FsPath, session: &str) -> String {
    rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap().query_row("SELECT connection_token FROM control_connections WHERE session_id=?1",
            [session], |row| row.get(0)).unwrap()
}

fn assert_live_original_settled(state: &AppState, original: &EngramCheckpointOperation) {
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    let canonical = loaded.engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.original.id == original.original.id).unwrap();
    canonical.validate().unwrap();
    assert_eq!(serde_json::to_value(&canonical.original).unwrap(), serde_json::to_value(&original.original).unwrap());
    assert!(matches!(canonical.attempts[0].result, Some(EngramCheckpointAttemptResult::Unknown { .. })));
    assert!(matches!(canonical.attempts.last().unwrap().result, Some(EngramCheckpointAttemptResult::Accepted)));
    assert!(matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. }));
    assert_eq!(canonical.protection.phase, EngramSessionObligationPhase::Confirmed);
    assert!(loaded.engram_confirmed_containment_ids.contains(&original.original.id));
    assert!(engram_session_obligation_locked(&loaded, &original.protection.local_session_id).is_none());
}

fn original_checkpoint_lost_reply_case(commit_before_loss: bool, restart_host: bool) {
    let binary = live_engram_binary();
    assert_eq!(sha256_hex(&fs::read(binary).unwrap()), CHECKPOINT_PRODUCER_SHA256,
        "this source-pair witness requires the reviewed producer artifact");
    let fixture = completion_fixture();
    let session = fixture.live.session_id.clone();
    let added = fixture.work(&session, &["add", "Recover the exact original checkpoint", "--json"]);
    let work = added["work"]["short_ref"].as_str().or_else(|| added["short_ref"].as_str()).unwrap();
    fixture.work(&session, &["claim", work, "--json"]);
    let transport = LostOriginalCheckpointTransport::new(fixture.live.transport.clone(), Some(commit_before_loss));
    fixture.live.state.install_test_engram_transport(transport.clone());
    fixture.begin(&session, "Deliver this original prompt only once.");
    fs::write(fixture.root.join("README.md"), "mutation retained in original checkpoint report\n").unwrap();
    let database = {
        let inner = fixture.live.state.inner.lock().unwrap();
        inner.find_project(&fixture.project_id).unwrap().engram.as_ref().unwrap()
            .authority_store_key.as_ref().unwrap().database_path.clone()
    };
    assert!(database.starts_with(&fixture.home), "only this disposable producer store is read");
    let old_token = producer_connection_token(&database, &session);
    finish_live_root(&fixture.live.state, &session);
    let original = fixture.record(&session, |record| record.engram.checkpoint_operation.clone().unwrap());
    original.validate().unwrap();
    assert!(original.has_uncertain_checkpoint_history());
    assert!(original.protection.authority_confirmed);
    assert!(!serde_json::to_value(&original.original.request).unwrap()["observations"].as_array().unwrap().is_empty(),
        "the replay must carry the real original report");
    let before_row = producer_checkpoint_row(&database, &original);
    assert_eq!(before_row.is_some(), commit_before_loss);
    let first_request = transport.checkpoint_requests.lock().unwrap()[0].clone();
    assert_eq!(first_request, serde_json::to_value(&original.original.request).unwrap());
    let admissions = fixture.live.transport.observations().iter().filter(|item|
        matches!(item.request["operation"].as_str(), Some("session_bind" | "turn_evaluate" | "turn_begin"))).count();
    if !restart_host {
        *transport.busy_database.lock().unwrap() = Some(database.clone());
        let busy = fixture.live.state.recover_original_checkpoint_result(&original.original.id);
        assert!(busy.is_err(), "a real held producer writer must withhold recovery");
        let held = fixture.record(&session, |record| record.engram.checkpoint_operation.clone().unwrap());
        assert!(held.has_uncertain_checkpoint_history());
        assert!(matches!(held.attempts.last().unwrap().result,
            Some(EngramCheckpointAttemptResult::CertifiedNotApplied { ref code }) if code == "control_store_busy"));
        assert_ne!(held.protection.phase, EngramSessionObligationPhase::Confirmed);
        assert_eq!(producer_checkpoint_row(&database, &original), before_row,
            "busy before lookup neither creates nor loses the original result");
        assert!(engram_session_obligation_locked(&fixture.live.state.inner.lock().unwrap(), &session).is_some());
    }
    let started = std::time::Instant::now();
    let mut restarted_receiver = None;
    let mut restarted_process = None;
    let (state, recovery, receiver) = if restart_host {
        fixture.live.state.shutdown_persist_blocking();
        let path = fixture.live.state.persistence_path.as_path().to_path_buf();
        let templates = fixture.live.state.orchestrator_templates_path.as_path().to_path_buf();
        let workdir = fixture.root.to_string_lossy().into_owned();
        let keep_root = fixture.live.state.test_temp_root.as_ref().unwrap().clone();
        drop(fixture);
        let inner = BoundaryFaultTransport::new(engram_host_workdir(&path).unwrap(), []);
        let recovery = LostOriginalCheckpointTransport::new(inner, None);
        let state = AppState::new_with_paths_and_engram_transport_for_test(workdir, path, templates, recovery.clone()).unwrap();
        let (runtime, provider_receiver, process) = test_shared_codex_runtime("original-checkpoint-restarted-provider");
        install_single_wire_codex_fixture(&state, Some(runtime));
        restarted_receiver = Some(provider_receiver);
        restarted_process = Some(process);
        // Retain the disposable stores for the reconstructed host's complete lifetime.
        let state = (state, keep_root);
        (state, recovery, None)
    } else {
        ((fixture.live.state.clone(), fixture.live.state.test_temp_root.as_ref().unwrap().clone()), transport.clone(), Some(fixture))
    };
    assert!(!state.0.inner.lock().unwrap().engram_confirmed_containment_ids.contains(&original.original.id),
        "construction alone must not settle the retained original");
    assert_eq!(recovery.checkpoint_requests.lock().unwrap().len(), if restart_host { 0 } else { 2 },
        "the explicit recovery trigger owns the next checkpoint invocation");
    assert!(state.0.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert_live_original_settled(&state.0, &original);
    let requests = recovery.checkpoint_requests.lock().unwrap().clone();
    assert_eq!(requests.len(), if restart_host { 1 } else { 3 });
    assert!(requests.iter().all(|request| request == &first_request));
    let after_row = producer_checkpoint_row(&database, &original).unwrap();
    let produced: EngramTurnCheckpointResponse = serde_json::from_slice(&after_row.2).unwrap();
    assert!(matches!(produced, EngramTurnCheckpointResponse::Checkpointed { receipt }
        if receipt.grant_id == original.protection.grant_id));
    if let Some(before_row) = before_row { assert_eq!(after_row, before_row, "cached producer result remains byte exact"); }
    assert_ne!(producer_connection_token(&database, &session), old_token, "a new process owns the old control identity without binding again");
    let current_admissions = recovery.inner.observations().iter().filter(|item|
        matches!(item.request["operation"].as_str(), Some("session_bind" | "turn_evaluate" | "turn_begin"))).count();
    assert_eq!(current_admissions, if restart_host { 0 } else { admissions });
    if let Some(fixture) = &receiver { assert!(fixture.live.receiver.try_recv().is_err()); }
    if let Some(receiver) = &restarted_receiver { assert!(receiver.try_recv().is_err()); }
    assert!(!state.0.recover_original_checkpoint_result(&original.original.id).unwrap(), "repeat accounting is idempotent");
    recovery.shutdown_session(&session);
    drop(restarted_process);
    eprintln!("original checkpoint producer recovery commit_before_loss={commit_before_loss} host_restart={restart_host} elapsed={:?}", started.elapsed());
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_original_checkpoint_uncommitted_lost_reply_recovers_after_process_restart() {
    original_checkpoint_lost_reply_case(false, false);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_original_checkpoint_uncommitted_lost_reply_recovers_after_host_restart() {
    original_checkpoint_lost_reply_case(false, true);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_original_checkpoint_committed_lost_reply_recovers_after_process_restart() {
    original_checkpoint_lost_reply_case(true, false);
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn live_original_checkpoint_committed_lost_reply_recovers_after_host_restart() {
    original_checkpoint_lost_reply_case(true, true);
}
