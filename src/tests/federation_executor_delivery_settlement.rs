// Real authenticated receive/loopback paths are distinguished below from
// injected persistence returns and synthetic positive runtime evidence.

fn wake_request(fixture: &Fixture) -> FederationExecutorWakeRequest {
    serde_json::from_value(fixture.request.clone()).unwrap()
}

fn ownership_receipt(fixture: &Fixture) -> FederationExecutorReceipt {
    let operation = fixture.operation();
    FederationExecutorReceipt { protocol_version: FEDERATION_PROTOCOL_VERSION,
        payload_digest: operation.digest(), operation, received: true }
}

fn pending(fixture: &Fixture) -> bool {
    fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT state='pending' FROM federation_mailbox_wake_obligations WHERE message_id=?1",
        [fixture.request["wake"]["messageId"].as_str().unwrap()], |row| row.get(0)).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_central_drain_has_authenticated_durable_receipt_and_preserves_append() {
    let fixture = Fixture::new();
    let check = fixture.channel.clone();
    let router = federation_router(fixture.channel.clone(),
        Arc::new(move || check.current_authority().map(|_| ())));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async { let _ = stopped.await; }).await.unwrap();
    });
    let channel = fixture.outbound.clone();
    let check = channel.clone();
    let rpc = FederationRpcClient::new(port, channel.clone(),
        Arc::new(move || check.current_authority().map(|_| ()))).unwrap();
    fixture.central.federation_peer_handles.lock().unwrap().insert(
        fixture.target.identity.host_id.clone(), Arc::new(FederationPeerHandle { channel, rpc }));
    let original: String = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT notification_disposition FROM mailbox_messages WHERE id=?1",
        [fixture.request["wake"]["messageId"].as_str().unwrap()], |row| row.get(0)).unwrap();
    fixture.central.drain_one_federation_executor_wake("").await;
    assert!(!pending(&fixture), "C settles only after authenticated matching durable S receipt");
    let operation = fixture.operation();
    let mirror = federation_executor_read(&fixture.central.mailbox_store.connection().unwrap(),
        &operation.central_host_id, &operation.operation_key).unwrap().unwrap();
    assert_eq!(mirror.operation, operation);
    assert_eq!((&*mirror.stage, &*mirror.effect_state), ("received", "not_started"));
    assert!(mirror.queue_entry_id.is_none() && mirror.runtime_ack.is_none());
    let after: String = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT notification_disposition FROM mailbox_messages WHERE id=?1",
        [fixture.request["wake"]["messageId"].as_str().unwrap()], |row| row.get(0)).unwrap();
    assert_eq!(after, original, "later wake state never rewrites the append disposition");
    assert_eq!(fixture.inbox().unwrap().stage, "received");
    assert!(fixture.queue(&fixture.satellite).is_empty(), "202 precedes local queue association");
    let mut activation = FederationExecutorActivationWorker::default();
    activation.tick(&fixture.satellite, true).await;
    activation.shutdown(&fixture.satellite).await;
    assert_eq!(fixture.queue(&fixture.satellite).len(), 1);
    assert!(fixture.inbox().unwrap().runtime_ack.is_none());
    let _ = stop.send(());
    server.await.unwrap();
    let central_cleaned = fixture.central.test_temp_root.as_ref().unwrap().observe_cleanup();
    let satellite_cleaned = fixture.satellite.test_temp_root.as_ref().unwrap().observe_cleanup();
    drop(fixture);
    phase_sync::receive(&central_cleaned, "central loopback fixture releases its state root").unwrap();
    phase_sync::receive(&satellite_cleaned, "satellite loopback fixture releases its state root").unwrap();
}

#[test]
fn federation_m4_runtime_mirror_commit_cuts_are_atomic_and_retry_exactly() {
    for cut in ["beforeMirrorCommit", "afterMirrorCommit"] {
        let fixture = Fixture::new();
        let request = wake_request(&fixture);
        let receipt = ownership_receipt(&fixture);
        fixture.central.mailbox_store.reserve_exact_federation_executor_wake(&request, || Ok(())).unwrap();
        FEDERATION_EXECUTOR_TEST_CUT.with(|slot| *slot.borrow_mut() = Some(cut));
        assert!(fixture.central.mailbox_store.settle_federation_executor_mirror(
            &request, &fixture.operation(), &receipt, || Ok(())).is_err());
        assert_eq!(pending(&fixture), cut == "beforeMirrorCommit");
        let mirror = federation_executor_read(&fixture.central.mailbox_store.connection().unwrap(),
            &receipt.operation.central_host_id, &receipt.operation.operation_key).unwrap();
        assert_eq!(mirror.is_some(), cut == "afterMirrorCommit");
        fixture.central.mailbox_store.settle_federation_executor_mirror(
            &request, &fixture.operation(), &receipt, || Ok(())).unwrap();
        fixture.central.mailbox_store.settle_federation_executor_mirror(
            &request, &fixture.operation(), &receipt, || Ok(())).unwrap();
        assert!(!pending(&fixture));
        assert!(fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap().is_empty());
    }
}

#[test]
fn federation_m4_runtime_mismatched_receipt_never_settles_or_writes_a_mirror() {
    let fixture = Fixture::new();
    let request = wake_request(&fixture);
    let valid = serde_json::to_value(ownership_receipt(&fixture)).unwrap();
    for (field, value) in [
        ("operationKey", json!("another-key")), ("targetHostId", json!("another-host")),
        ("centralHostId", json!("another-central")), ("centralEpoch", json!(99)),
        ("enrollmentGeneration", json!(99)), ("sessionId", json!("another-session")),
        ("creationId", json!("another-creation")), ("placementGeneration", json!(99)),
        ("payload", json!("different content")),
    ] {
        let mut changed = valid.clone();
        changed["operation"][field] = value;
        let receipt = serde_json::from_value(changed).unwrap();
        assert!(fixture.central.mailbox_store.settle_federation_executor_mirror(
            &request, &fixture.operation(), &receipt, || Ok(())).is_err(), "{field}");
        assert!(pending(&fixture));
    }
    let mut receipt = ownership_receipt(&fixture);
    receipt.payload_digest = "0".repeat(64);
    assert!(fixture.central.mailbox_store.settle_federation_executor_mirror(
        &request, &fixture.operation(), &receipt, || Ok(())).is_err());
    assert_eq!(fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_executor_operations", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_mirror_cannot_advance_or_dispatch_and_wrong_host_is_refused() {
    let fixture = Fixture::new();
    let request = wake_request(&fixture);
    let receipt = ownership_receipt(&fixture);
    fixture.central.mailbox_store.settle_federation_executor_mirror(
        &request, &fixture.operation(), &receipt, || Ok(())).unwrap();
    let host = fixture.target.central.host_id.clone();
    assert!(fixture.central.mailbox_store.record_federation_executor_evidence(&host, &fixture.operation(),
        FederationExecutorEvidence::Queued { queue_entry_id: "forbidden".into(), delivery_attempt_id: "forbidden".into() },
        || Ok(())).is_err());
    FederationExecutorRecovery::default().recover(&fixture.central);
    assert!(fixture.central.mailbox_store.local_federation_executor_recovery(
        &host, &host, "").unwrap().is_empty());
    assert!(fixture.central.queue_federation_executor_operation(&fixture.operation(), true).is_err());
    let mut wrong = fixture.request.clone();
    wrong["target"]["identity"]["hostId"] = json!("different-satellite");
    let (status, body) = fixture.receive(&wrong).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "malformed host identity: {body}");
    assert_eq!(body, json!({"error": "invalid federation host identity"}));
    // A valid, distinct host reaches the current-target guard instead.
    assert_ne!(fixture.target.central.host_id, fixture.target.identity.host_id);
    wrong["target"]["identity"]["hostId"] = json!(fixture.target.central.host_id);
    let (status, body) = fixture.receive(&wrong).await;
    assert_eq!(status, StatusCode::CONFLICT, "valid but different target host: {body}");
    assert!(fixture.inbox().is_none());
}

#[test]
fn federation_m4_runtime_capacity_reservations_survive_interleaved_lost_replies_and_settlement() {
    // Deterministic pseudo-random interleavings exercise production arithmetic.
    // Settled mirrors retain exactly the bytes that their pending slots owned.
    for seed in 1..=12u64 {
        let mut random = seed;
        let mut rows = Vec::<(u64, bool, bool)>::new(); // bytes, dispatched, settled
        for _ in 0..5000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            match (random >> 32) % 4 {
                0 | 1 => rows.push((1 + random % FEDERATION_EXECUTOR_MAX_PAYLOAD as u64, false, false)),
                2 => {
                    let settled = rows.iter().filter(|row| row.2).collect::<Vec<_>>();
                    let count = settled.len() as u64;
                    let bytes = settled.iter().map(|row| row.0).sum();
                    let limit = federation_executor_reserved_prefix(count, bytes,
                        rows.iter().filter(|row| !row.2).map(|row| row.0));
                    if let Some(row) = rows.iter_mut().filter(|row| !row.2).take(limit)
                        .nth((random as usize >> 8) % limit.max(1)) { row.1 = true; }
                    // A lost response deliberately leaves the dispatched row pending.
                }
                _ => {
                    if let Some(row) = rows.iter_mut().find(|row| row.1 && !row.2) { row.2 = true; }
                }
            }
            let settled = rows.iter().filter(|row| row.2).collect::<Vec<_>>();
            let limit = federation_executor_reserved_prefix(settled.len() as u64,
                settled.iter().map(|row| row.0).sum(), rows.iter().filter(|row| !row.2).map(|row| row.0));
            assert!(rows.iter().filter(|row| !row.2).skip(limit).all(|row| !row.1),
                "a dispatched operation lost its reservation, seed {seed}");
        }
    }
}

#[test]
fn federation_m4_runtime_reservation_payload_bytes_match_the_real_encoder() {
    let fixture = Fixture::new();
    let requests = fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].operation(fixture.operation().enrollment_generation).unwrap(), fixture.operation());
    let operation = fixture.operation();
    fixture.central.mailbox_store.settle_federation_executor_mirror(
        &requests[0], &operation, &ownership_receipt(&fixture), || Ok(())).unwrap();
    let bytes: u64 = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT length(CAST(payload AS BLOB)) FROM federation_executor_operations WHERE operation_key=?1",
        [&operation.operation_key], |row| row.get(0)).unwrap();
    assert_eq!(bytes, operation.payload.len() as u64);
}

#[test]
fn federation_m4_runtime_queue_boundaries_reopen_one_exact_association_without_resume() {
    for phase in ["beforeQueueAssociation", "afterQueueAssociationCommit",
        "afterQueueEvidenceCommit", "afterQueueDurabilityAck"]
    {
        let fixture = Fixture::new();
        let operation = fixture.operation();
        fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
        FEDERATION_EXECUTOR_TEST_CUT.with(|slot| *slot.borrow_mut() = Some(phase));
        assert!(fixture.satellite.queue_federation_executor_operation(&operation, false).is_err(), "{phase}");
        let reloaded = fixture.reload();
        let queue = fixture.queue(&reloaded.0);
        assert_eq!(queue.len(), 1, "{phase}");
        assert_eq!(queue[0].pending_prompt.id, operation.queue_identity());
        let journal = federation_executor_read(&reloaded.0.mailbox_store.connection().unwrap(),
            &operation.central_host_id, &operation.operation_key).unwrap().unwrap();
        assert_eq!(journal.stage, "queued");
        assert_eq!(journal.effect_state, "not_started");
        let inner = reloaded.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert_eq!(record.prompt_deliveries.len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_unknown_handoff_survives_restart_and_ack_without_reexecution() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let operation = fixture.operation();
    let prompt = operation.queue_identity();
    let host = operation.target_host_id.clone();
    fixture.satellite.mailbox_store.record_federation_executor_evidence(&host, &operation,
        FederationExecutorEvidence::EffectAttempt { delivery_attempt_id: prompt.clone() }, || Ok(())).unwrap();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &mut inner.sessions[index];
        let receipt = record.prompt_deliveries.get_mut(&prompt).unwrap();
        receipt.disposition = PromptDeliveryDisposition::HandingOff;
        receipt.turn_generation = Some(1);
        receipt.runtime_write = PromptRuntimeWrite::Begun;
        receipt.federation_handoff_started = true;
    }
    let reloaded = fixture.reload();
    assert!(fixture.queue(&reloaded.0).is_empty(), "unknown effects must not become a new queue entry");
    {
        let inner = reloaded.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        assert_eq!(record.prompt_deliveries[&prompt].disposition, PromptDeliveryDisposition::Ambiguous);
    }
    // Synthetic already-validated central ACK evidence, deliberately separate
    // from runtime acceptance. Production RPC validation is tested separately.
    let row = reloaded.0.mailbox_store.record_federation_executor_evidence(&host, &operation,
        FederationExecutorEvidence::Processed { kind: FederationExecutorProcessedKind::MailboxAck,
            evidence: "validated-central-ack".into() }, || Ok(())).unwrap();
    assert_eq!(row.stage, "processed");
    assert_eq!(row.effect_state, "unknown");
    assert!(row.runtime_ack.is_none());
    FederationExecutorRecovery::default().recover(&reloaded.0);
    assert!(fixture.queue(&reloaded.0).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_positive_runtime_receipt_reconciles_a_lost_session_settlement() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let operation = fixture.operation();
    let prompt = operation.queue_identity();
    let host = operation.target_host_id.clone();
    fixture.satellite.mailbox_store.record_federation_executor_evidence(&host, &operation,
        FederationExecutorEvidence::EffectAttempt { delivery_attempt_id: prompt.clone() }, || Ok(())).unwrap();
    // Inject positive runtime evidence at the storage seam, without claiming
    // that this test launched a provider or observed a provider acknowledgement.
    fixture.satellite.mailbox_store.record_federation_executor_evidence(&host, &operation,
        FederationExecutorEvidence::RuntimeAccepted { delivery_attempt_id: prompt.clone(),
            acknowledgement: format!("runtime-accepted:{prompt}") }, || Ok(())).unwrap();
    let reloaded = fixture.reload();
    assert!(fixture.queue(&reloaded.0).is_empty());
    let inner = reloaded.0.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
    assert_eq!(record.prompt_deliveries[&prompt].disposition, PromptDeliveryDisposition::Accepted);
    assert!(record.orchestrator_auto_dispatch_blocked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_accepted_session_receipt_survives_a_lost_journal_settlement() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let operation = fixture.operation();
    let prompt = operation.queue_identity();
    fixture.satellite.mailbox_store.record_federation_executor_evidence(&operation.target_host_id, &operation,
        FederationExecutorEvidence::EffectAttempt { delivery_attempt_id: prompt.clone() }, || Ok(())).unwrap();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &mut inner.sessions[index];
        record.queued_prompts.clear();
        record.prompt_deliveries.get_mut(&prompt).unwrap().disposition = PromptDeliveryDisposition::Accepted;
    }
    let reloaded = fixture.reload();
    let journal = federation_executor_read(&reloaded.0.mailbox_store.connection().unwrap(),
        &operation.central_host_id, &operation.operation_key).unwrap().unwrap();
    assert_eq!(journal.stage, "runtime_accepted");
    assert_eq!(journal.effect_state, "confirmed");
    assert!(fixture.queue(&reloaded.0).is_empty());
}

#[test]
fn federation_m4_runtime_capacity_refuses_dispatch_without_changing_the_append() {
    let fixture = Fixture::new();
    let mut operation = fixture.operation();
    operation.operation_key = "capacity-fixture".into();
    fixture.central.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    // Bounded valid pressure fixture. No payload or unresolved row is evicted.
    fixture.central.mailbox_store.connection().unwrap().execute(
        "WITH RECURSIVE slots(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM slots WHERE n<?1)
         INSERT INTO federation_executor_operations(central_host_id,operation_key,central_epoch,
         target_host_id,enrollment_generation,session_id,creation_id,placement_generation,command_kind,
         payload_digest,payload,payload_bytes,stage,effect_state)
         SELECT central_host_id,'capacity-'||n,central_epoch,target_host_id,enrollment_generation,
         session_id,creation_id,placement_generation,command_kind,payload_digest,payload,payload_bytes,
         'received','not_started' FROM federation_executor_operations,slots WHERE operation_key='capacity-fixture'",
        [FEDERATION_EXECUTOR_MAX_COUNT - 1]).unwrap();
    assert!(fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap().is_empty());
    let error = fixture.central.mailbox_store.reserve_exact_federation_executor_wake(
        &wake_request(&fixture), || Ok(())).unwrap_err();
    assert_eq!(error.downcast_ref::<MailboxStoreError>().unwrap().kind, MailboxStoreErrorKind::Capacity);
    assert!(pending(&fixture));
    assert!(fixture.inbox().is_none(), "an unreserved operation is never sent to S");
    assert!(fixture.central.mailbox_store.federation_executor_wake_state(
        fixture.request["wake"]["messageId"].as_str().unwrap()).unwrap().contains("reason capacity"));
}

#[test]
fn federation_m4_runtime_abrupt_central_exit_preserves_atomic_receiver_ownership() {
    const DATA: &str = "TERMAL_EXECUTOR_MIRROR_CRASH_DATA";
    const DATABASE: &str = "TERMAL_EXECUTOR_MIRROR_CRASH_DATABASE";
    const PHASE: &str = "TERMAL_EXECUTOR_MIRROR_CRASH_PHASE";
    if let (Ok(data), Some(database), Ok(phase)) =
        (std::env::var(DATA), std::env::var_os(DATABASE), std::env::var(PHASE))
    {
        let (request, receipt): (FederationExecutorWakeRequest, FederationExecutorReceipt) =
            serde_json::from_str(&data).unwrap();
        let phase = match phase.as_str() {
            "beforeMirrorCommit" => "beforeMirrorCommit",
            "afterMirrorCommit" => "afterMirrorCommit",
            _ => panic!("invalid isolated mirror crash phase"),
        };
        let store = MailboxStore::open(FsPath::new(&database)).unwrap();
        FEDERATION_EXECUTOR_TEST_CUT.with(|slot| *slot.borrow_mut() = Some(phase));
        FEDERATION_EXECUTOR_TEST_CRASH.with(|slot| slot.set(true));
        store.settle_federation_executor_mirror(&request, &receipt.operation, &receipt, || Ok(())).unwrap();
        panic!("the isolated child must exit inside the production transaction boundary");
    }
    for phase in ["beforeMirrorCommit", "afterMirrorCommit"] {
        let fixture = Fixture::new();
        let request = wake_request(&fixture);
        let receipt = ownership_receipt(&fixture);
        fixture.satellite.mailbox_store.admit_federation_executor_operation(&fixture.operation(), || Ok(())).unwrap();
        fixture.central.mailbox_store.reserve_exact_federation_executor_wake(&request, || Ok(())).unwrap();
        let path = resolve_coordination_persistence_path(fixture.central.persistence_path.as_path());
        let module = module_path!().split_once("::").unwrap().1;
        let filter = format!("{module}::federation_m4_runtime_abrupt_central_exit_preserves_atomic_receiver_ownership");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg(filter).arg("--nocapture")
            .env(DATA, serde_json::to_string(&(&request, &receipt)).unwrap())
            .env(DATABASE, &path).env(PHASE, phase)
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() { break status; }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("isolated mirror crash child did not reach {phase}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.code(), Some(73), "{phase}");
        let reopened = MailboxStore::open(&path).unwrap();
        let mirror = federation_executor_read(&reopened.connection().unwrap(),
            &receipt.operation.central_host_id, &receipt.operation.operation_key).unwrap();
        assert_eq!(mirror.is_some(), phase == "afterMirrorCommit");
        assert_eq!(pending(&fixture), phase == "beforeMirrorCommit");
        reopened.settle_federation_executor_mirror(&request, &receipt.operation, &receipt, || Ok(())).unwrap();
        assert!(!pending(&fixture));
        assert_eq!(fixture.inbox().unwrap().operation, receipt.operation);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_provider_handoff_is_once_and_queueing_is_not_a_runtime_ack() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let operation = fixture.operation();
    let prompt = operation.queue_identity();
    let (runtime, input) = test_claude_runtime_handle("federation-executor-once");
    let token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let sender = runtime.input_tx.clone();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Claude(runtime);
        record.session.status = SessionStatus::Active;
        record.active_turn_generation = 1;
        let receipt = record.prompt_deliveries.get_mut(&prompt).unwrap();
        receipt.disposition = PromptDeliveryDisposition::HandingOff;
        receipt.turn_generation = Some(1);
        receipt.write_ahead_acknowledged = true;
        fixture.satellite.commit_locked(&mut inner).unwrap();
    }
    let dispatch = || TurnDispatch::PersistentClaude {
        federation_handoff: {
            let inner = fixture.satellite.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
            let receipt = &record.prompt_deliveries[&fixture.operation().queue_identity()];
            Some(FederationExecutorHandoffIdentity { operation: fixture.operation(),
                prompt_id: fixture.operation().queue_identity(), write_ahead: receipt.write_ahead.clone(),
                server_instance_id: fixture.satellite.server_instance_id.clone() })
        },
        active_turn_generation: 1, opening_prompt: None,
        command: ClaudePromptCommand { attachments: Vec::new(), replay_generation: "one-logical-effect".into(),
            text: "ordinary mailbox wake".into(), turn_generation: 1 },
        engram_dispatch_generation: None, mailbox_notification: None, runtime_token: token.clone(),
        sender: sender.clone(), session_id: fixture.native.clone(),
    };
    assert!(matches!(handoff_prepared_turn_dispatch(&fixture.satellite, dispatch()).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered));
    assert!(matches!(handoff_prepared_turn_dispatch(&fixture.satellite, dispatch()).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered));
    assert!(matches!(input.try_recv().unwrap(), ClaudeRuntimeCommand::Prompt(_)));
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let journal = fixture.inbox().unwrap();
    assert_eq!(journal.effect_state, "unknown");
    assert!(journal.runtime_ack.is_none(), "provider enqueue is not a runtime acceptance");
    fixture.satellite.note_prompt_delivery_outcome(&fixture.native, Some(1), PromptDeliveryOutcome::Accepted);
    assert_eq!(fixture.inbox().unwrap().effect_state, "confirmed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_only_a_validated_central_ack_advances_processing() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let operation = fixture.operation();
    let prompt = operation.queue_identity();
    fixture.satellite.mailbox_store.record_federation_executor_evidence(&operation.target_host_id, &operation,
        FederationExecutorEvidence::EffectAttempt { delivery_attempt_id: prompt }, || Ok(())).unwrap();
    let channel = fixture.outbound.clone();
    let check = channel.clone();
    let router = federation_router(channel, Arc::new(move || check.current_authority().map(|_| ())));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async { let _ = stopped.await; }).await.unwrap();
    });
    let check = fixture.channel.clone();
    let rpc = FederationRpcClient::new(port, fixture.channel.clone(),
        Arc::new(move || check.current_authority().map(|_| ()))).unwrap();
    fixture.satellite.federation_peer_handles.lock().unwrap().insert(fixture.target.central.host_id.clone(),
        Arc::new(FederationPeerHandle { channel: fixture.channel.clone(), rpc }));
    let mailbox = fixture.request["wake"]["mailboxId"].as_str().unwrap().to_owned();
    let refused: Result<MailboxSummary, ApiError> = fixture.satellite.federation_mailbox_operation(
        &fixture.native, FederationMailboxOperation::Acknowledge { mailbox_id: mailbox.clone(), receipt: "unissued".into() }).await;
    assert!(refused.is_err());
    assert!(fixture.inbox().unwrap().processed_evidence.is_none());
    let page: MailboxReadResponse = fixture.satellite.federation_mailbox_operation(&fixture.native,
        FederationMailboxOperation::Read { mailbox_id: mailbox.clone(), after_sequence: None,
            limit: 200, issue_receipt: true }).await.unwrap();
    let before = fixture.queue(&fixture.satellite);
    let _: MailboxSummary = fixture.satellite.federation_mailbox_operation(&fixture.native,
        FederationMailboxOperation::Acknowledge { mailbox_id: mailbox, receipt: page.receipt.unwrap() }).await.unwrap();
    let journal = fixture.inbox().unwrap();
    assert_eq!(journal.stage, "processed");
    assert_eq!(journal.effect_state, "unknown");
    assert!(journal.runtime_ack.is_none());
    let after = fixture.queue(&fixture.satellite);
    assert_eq!(after.len(), before.len(), "ACK preserves an already owed continuation");
    assert_eq!(serde_json::to_value(&after[0].pending_prompt.source).unwrap(),
        serde_json::to_value(&before[0].pending_prompt.source).unwrap());
    let _ = stop.send(());
    server.await.unwrap();
    let central_cleaned = fixture.central.test_temp_root.as_ref().unwrap().observe_cleanup();
    let satellite_cleaned = fixture.satellite.test_temp_root.as_ref().unwrap().observe_cleanup();
    drop(fixture);
    phase_sync::receive(&central_cleaned, "central ACK fixture releases its state root").unwrap();
    phase_sync::receive(&satellite_cleaned, "satellite ACK fixture releases its state root").unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_receipt_pruning_and_cancel_cannot_recreate_a_remote_wake() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let operation = fixture.operation();
    let prompt = operation.queue_identity();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &mut inner.sessions[index];
        let mut ordinary = record.prompt_deliveries[&prompt].clone();
        ordinary.federation_operation = None;
        ordinary.disposition = PromptDeliveryDisposition::Accepted;
        for number in 0..(PROMPT_DELIVERY_SETTLED_LIMIT + 10) {
            ordinary.sequence = number as u64 + 100;
            record.prompt_deliveries.insert(format!("ordinary-{number}"), ordinary.clone());
        }
        record.queued_prompts.clear(); // local cancellation retires the original queue entry
        prune_prompt_deliveries(record);
        assert!(record.prompt_deliveries.contains_key(&prompt), "the remote identity outlives display pruning");
        fixture.satellite.commit_locked(&mut inner).unwrap();
    }
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    assert!(fixture.queue(&fixture.satellite).is_empty(), "duplicate ownership is not permission to undo cancellation");
    let reloaded = fixture.reload();
    assert!(fixture.queue(&reloaded.0).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_runtime_sender_recovery_reuses_a_possibly_committed_original_intent() {
    let fixture = Fixture::new();
    let sender = {
        let inner = fixture.central.inner.lock().unwrap();
        inner.federation.mailbox_directory[fixture.request["wake"]["senderSessionId"].as_str().unwrap()]
            .binding.identity.session_id.clone()
    };
    let delivery = fixture.central.prepare_federation_delivery(&sender, FederationMailboxOperation::Send {
        target: fixture.target.participant_key(), message: "ordinary agent work".into(), key: "one-message".into(),
        topic: Some("handoff".into()), state_stamp: None,
    }).await.unwrap();
    let creation = fixture.central.federation_intent_owner(&sender).unwrap();
    fixture.central.mailbox_store.mark_federation_intent_unknown(&delivery,
        || fixture.central.lock_federation_intent_owner(&sender, &creation, &delivery.sender)).unwrap();
    let mut recovery = FederationExecutorSenderRecovery::default();
    recovery.tick(&fixture.central).await;
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            if fixture.central.local_federation_intent(&sender, &delivery.key).unwrap().state == "homeCommitted" { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("retained sender intent must reconcile its exact committed receipt");
    let connection = fixture.central.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [],
        |row| row.get::<_, u64>(0)).unwrap(), 1);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM federation_mailbox_wake_obligations", [],
        |row| row.get::<_, u64>(0)).unwrap(), 1);
    drop(connection);
    let receipt = fixture.central.local_federation_intent(&sender, &delivery.key).unwrap()
        .committed_receipt().unwrap().unwrap();
    assert_eq!(receipt.message_id, fixture.request["wake"]["messageId"].as_str().unwrap());
    recovery.shutdown().await;
}
