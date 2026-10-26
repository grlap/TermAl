// Reproductions of production recovery boundaries. Provider channels below are
// real local channels; record reload uses the production persisted conversion.
// Neither is a provider OS-crash or cross-machine durability proof.

#[path = "federation_executor_round_two.rs"]
mod round_two;
#[path = "federation_executor_round_three.rs"]
mod round_three;
#[path = "federation_executor_round_four.rs"]
mod round_four;
#[path = "federation_executor_round_five.rs"]
mod round_five;
#[path = "federation_executor_live_wake.rs"]
mod live_wake;
#[path = "federation_executor_outcomes.rs"]
mod outcomes;

fn append_wake(fixture: &Fixture, name: &str, topic: &str) -> String {
    let (sender, _) = federation_mailboxes::local_participant(&fixture.central, name);
    let response = fixture.central.execute_federation_mailbox(federation_mailboxes::request(
        &fixture.central, &sender, FederationMailboxOperation::Send {
            target: fixture.target.participant_key(), message: "retained ordinary work".into(),
            key: "regression-message".into(), topic: Some(topic.into()), state_stamp: None,
        },
    ), None).unwrap().value;
    response["messageId"].as_str().unwrap().to_owned()
}

async fn attach_loopback(fixture: &Fixture)
    -> (tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>)
{
    let check = fixture.channel.clone();
    let router = federation_router(fixture.channel.clone(),
        Arc::new(move || check.current_authority().map(|_| ())));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async { let _ = stopped.await; })
            .await.unwrap();
    });
    let channel = fixture.outbound.clone();
    let check = channel.clone();
    let rpc = FederationRpcClient::new(port, channel.clone(),
        Arc::new(move || check.current_authority().map(|_| ()))).unwrap();
    fixture.central.federation_peer_handles.lock().unwrap().insert(
        fixture.target.identity.host_id.clone(), Arc::new(FederationPeerHandle { channel, rpc }));
    (stop, server)
}

fn prepare_channel(fixture: &Fixture, label: &str, turn: u64)
    -> (RuntimeToken, mpsc::Sender<ClaudeRuntimeCommand>, mpsc::Receiver<ClaudeRuntimeCommand>)
{
    let (runtime, input) = test_claude_runtime_handle(label);
    let token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let sender = runtime.input_tx.clone();
    let mut inner = fixture.satellite.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.native).unwrap();
    let record = &mut inner.sessions[index];
    record.runtime = SessionRuntime::Claude(runtime);
    record.session.status = SessionStatus::Active;
    record.active_turn_generation = turn;
    let receipt = record.prompt_deliveries.get_mut(&fixture.operation().queue_identity()).unwrap();
    receipt.disposition = PromptDeliveryDisposition::HandingOff;
    receipt.turn_generation = Some(turn);
    receipt.write_ahead_acknowledged = true;
    // Model the production write-ahead callback: this exact record is already
    // committed before the final provider enqueue, including after replacement.
    fixture.satellite.commit_locked(&mut inner).unwrap();
    (token, sender, input)
}

fn channel_dispatch(fixture: &Fixture, token: &RuntimeToken,
    sender: &mpsc::Sender<ClaudeRuntimeCommand>, turn: u64) -> TurnDispatch
{
    let federation_handoff = {
        let inner = fixture.satellite.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        record.prompt_deliveries.get(&fixture.operation().queue_identity())
            .filter(|receipt| receipt.disposition == PromptDeliveryDisposition::HandingOff
                && receipt.turn_generation == Some(turn))
            .map(|receipt| FederationExecutorHandoffIdentity {
                operation: receipt.federation_operation.clone().unwrap(),
                prompt_id: fixture.operation().queue_identity(), write_ahead: receipt.write_ahead.clone(),
                server_instance_id: fixture.satellite.server_instance_id.clone(),
            })
    };
    TurnDispatch::PersistentClaude {
        federation_handoff,
        active_turn_generation: turn, opening_prompt: None,
        command: ClaudePromptCommand { attachments: Vec::new(), replay_generation: format!("attempt-{turn}"),
            text: "ordinary mailbox wake".into(), turn_generation: turn },
        engram_dispatch_generation: None, mailbox_notification: None, runtime_token: token.clone(),
        sender: sender.clone(), session_id: fixture.native.clone(),
    }
}

fn retain_only_automatic_binding(fixture: &Fixture) {
    let mut inner = fixture.satellite.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.native).unwrap();
    let root = inner.session_mut_by_index(index).unwrap();
    root.federation_automatic_mailbox_binding = root.federation_mailbox_binding.take();
    assert_eq!(root.mailbox_binding(), Some(&fixture.target));
    assert!(root.federation_mailbox_binding.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_mailbox_binding_cannot_recover_a_retained_executor_operation() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    // Seed a received journal row without a queue association, as at a crash
    // boundary. Recovery must judge current explicit authority independently.
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    retain_only_automatic_binding(&fixture);
    {
        // Exercise the persisted shape that a restored journal encounters.
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index] = PersistedSessionRecord::from_record(&inner.sessions[index])
            .into_record().unwrap();
        assert!(inner.sessions[index].federation_mailbox_binding.is_none());
        assert_eq!(inner.sessions[index].mailbox_binding(), Some(&fixture.target));
    }
    assert!(fixture.queue(&fixture.satellite).is_empty());
    FederationExecutorRecovery::default().recover(&fixture.satellite);
    assert!(fixture.queue(&fixture.satellite).is_empty());
    let journal = fixture.inbox().unwrap();
    assert!(journal.queue_entry_id.is_none());
    assert_eq!(journal.effect_state, "not_started");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_mailbox_binding_cannot_handoff_a_prepared_executor_prompt() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let (token, sender, input) = prepare_channel(&fixture, "automatic-only-runtime", 1);
    let dispatch = channel_dispatch(&fixture, &token, &sender, 1);
    assert!(dispatch.federation_handoff().is_some(), "exercise the remote provider boundary");
    retain_only_automatic_binding(&fixture);
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        let root = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        assert!(root.runtime.matches_runtime_token(&token), "keep the prepared provider attempt current");
        assert_eq!(root.active_turn_generation, 1);
        assert_eq!(root.prompt_deliveries[&fixture.operation().queue_identity()].disposition,
            PromptDeliveryDisposition::HandingOff);
    }
    let result = handoff_prepared_turn_dispatch(&fixture.satellite, dispatch);
    assert!(result.is_err(), "mailbox routing must not authorize provider handoff");
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let journal = fixture.inbox().unwrap();
    assert_eq!(journal.effect_state, "not_started");
    assert!(journal.runtime_ack.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_review_legal_topic_cannot_poison_unrelated_wake_reservations() {
    let fixture = Fixture::new();
    let unusual = append_wake(&fixture, "Sender", "a\0b");
    let ordinary = append_wake(&fixture, "Another sender", "ordinary topic");
    let (stop, server) = attach_loopback(&fixture).await;
    // Exercise the production append, reservation, authenticated receiver and
    // ownership settlement. No stored mailbox row is changed by the fixture.
    let reserved = fixture.central.mailbox_store.reserve_federation_executor_wakes()
        .expect("one legal topic must not abort every reservation");
    assert!(reserved.iter().any(|row| row.wake.message_id == unusual));
    assert!(reserved.iter().any(|row| row.wake.message_id == ordinary));
    for request in reserved {
        fixture.central.dispatch_federation_executor_wake(request).await.unwrap();
    }
    let connection = fixture.central.mailbox_store.connection().unwrap();
    let pending: u64 = connection.query_row(
        "SELECT COUNT(*) FROM federation_mailbox_wake_obligations WHERE state='pending'", [],
        |row| row.get(0)).unwrap();
    assert_eq!(pending, 0);
    let _ = stop.send(());
    drop(connection);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_review_display_metadata_fits_request_and_echoed_receipt_bounds() {
    let fixture = Fixture::new();
    let large_name = "\u{1}".repeat(1024);
    let large_topic = "\u{2}".repeat(MAX_MAILBOX_METADATA_BYTES);
    let message = append_wake(&fixture, &large_name, &large_topic);
    let (stop, server) = attach_loopback(&fixture).await;
    let requests = fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap();
    let request = requests.into_iter().find(|row| row.wake.message_id == message).unwrap();
    fixture.central.dispatch_federation_executor_wake(request).await
        .expect("accepted mailbox metadata must fit both bounded wire directions");
    let _ = stop.send(());
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_review_unsent_retained_handoff_retries_under_its_fresh_nonce() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let prompt = fixture.operation().queue_identity();
    let (token, sender, input) = prepare_channel(&fixture, "unsent-old-runtime", 1);
    drop(input); // A definite channel rejection, before any provider can read.
    let dispatch = channel_dispatch(&fixture, &token, &sender, 1);
    let identity = dispatch.federation_handoff().cloned();
    let result = handoff_prepared_turn_dispatch(&fixture.satellite, dispatch);
    assert!(result.is_err(), "closed channel must prove the first attempt unsent");
    fixture.satellite.note_direct_hand_off_outcome_with_federation(&fixture.native, 1, &result, identity.as_ref());
    let first_nonce = {
        let inner = fixture.satellite.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        assert_eq!(record.prompt_deliveries[&prompt].disposition, PromptDeliveryDisposition::Pending,
            "the retained queue head must survive definite local rejection");
        record.prompt_deliveries[&prompt].write_ahead.clone()
    };
    let (token, sender, input) = prepare_channel(&fixture, "unsent-new-runtime", 2);
    let result = handoff_prepared_turn_dispatch(&fixture.satellite,
        channel_dispatch(&fixture, &token, &sender, 2));
    assert!(matches!(result.unwrap(), HandoffPreparedTurnDispatchOutcome::Delivered));
    assert!(matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))),
        "the old nonce's latch must not suppress the replacement channel send");
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let inner = fixture.satellite.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
    assert_eq!(record.prompt_deliveries[&prompt].write_ahead, first_nonce);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_review_recovery_of_unchanged_paused_queue_is_read_only() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let revision = fixture.satellite.inner.lock().unwrap().revision;
    let changes: u64 = fixture.satellite.mailbox_store.connection().unwrap()
        .query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap();
    let mut recovery = FederationExecutorRecovery::default();
    for _ in 0..4 { recovery.recover(&fixture.satellite); }
    let after: u64 = fixture.satellite.mailbox_store.connection().unwrap()
        .query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap();
    assert_eq!(fixture.satellite.inner.lock().unwrap().revision, revision,
        "an unchanged paused queue must not publish another state every sweep");
    assert_eq!(after, changes, "unchanged Queued evidence must not write again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_review_completed_receipts_do_not_rewrite_on_recovery_sweeps() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let (token, sender, input) = prepare_channel(&fixture, "accepted-runtime", 1);
    handoff_prepared_turn_dispatch(&fixture.satellite,
        channel_dispatch(&fixture, &token, &sender, 1)).unwrap();
    assert!(matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    fixture.satellite.note_prompt_delivery_outcome(&fixture.native, Some(1), PromptDeliveryOutcome::Accepted);
    let changes: u64 = fixture.satellite.mailbox_store.connection().unwrap()
        .query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap();
    let mut recovery = FederationExecutorRecovery::default();
    for _ in 0..4 { recovery.recover(&fixture.satellite); }
    let after: u64 = fixture.satellite.mailbox_store.connection().unwrap()
        .query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap();
    assert_eq!(after, changes, "a positive receipt already reconciled to both stores stays idle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_review_one_drain_pass_delivers_a_bounded_burst_without_waiting_a_tick_per_wake() {
    let fixture = Fixture::new();
    append_wake(&fixture, "Second sender", "second");
    append_wake(&fixture, "Third sender", "third");
    let (stop, server) = attach_loopback(&fixture).await;
    let mut drain = FederationExecutorWakeDrain::default();
    drain.tick(&fixture.central);
    let done = drain.pending.take().expect("one owned drain pass");
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, done).await.unwrap().unwrap();
    let pending: u64 = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_wake_obligations WHERE state='pending'", [],
        |row| row.get(0)).unwrap();
    assert_eq!(pending, 0, "three ready peers' wakes must not need three 15-second supervisor ticks");
    let _ = stop.send(());
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_review_reopened_queue_can_handoff_before_background_journal_recovery() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    FEDERATION_EXECUTOR_TEST_CUT.with(|slot| *slot.borrow_mut() = Some("afterQueueAssociationCommit"));
    assert!(fixture.satellite.queue_federation_executor_operation(&operation, false).is_err());
    assert!(fixture.inbox().unwrap().queue_entry_id.is_none());
    let prompt = operation.queue_identity();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        // Production reload conversion, before the next background recovery
        // page. This does not invoke boot recovery ahead of the counterexample.
        inner.sessions[index] = PersistedSessionRecord::from_record(&inner.sessions[index])
            .into_record().unwrap();
        assert!(inner.sessions[index].queued_prompt_durability_holds[&prompt].acknowledged);
    }
    let (token, sender, input) = prepare_channel(&fixture, "reopened-runtime", 1);
    let result = handoff_prepared_turn_dispatch(&fixture.satellite,
        channel_dispatch(&fixture, &token, &sender, 1));
    assert!(result.is_ok(), "the first natural handoff must repair the durable Queued gap, not reject: {result:?}");
    assert!(matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    let journal = fixture.inbox().unwrap();
    assert_eq!(journal.queue_entry_id.as_deref(), Some(prompt.as_str()));
    assert_eq!(journal.effect_state, "unknown");
}

#[test]
fn federation_m4_review_invalid_prefix_scan_advances_in_bounded_pages() {
    let fixture = Fixture::new();
    // Minimal read fixture for the production scan algorithm, not migration
    // or end-to-end durability proof. More than one pass of excluded rows.
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch(
        "CREATE TABLE mailbox_messages(id TEXT PRIMARY KEY,mailbox_id TEXT,sequence INTEGER,
         unread_depth_at_append INTEGER,sender_session_id TEXT,sender_name TEXT,topic TEXT);
         CREATE TABLE federation_mailbox_wake_obligations(message_id TEXT PRIMARY KEY,
         target_binding_json TEXT,state TEXT);
         CREATE TABLE federation_executor_operations(payload_bytes INTEGER);
         WITH RECURSIVE rows(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM rows WHERE n<1050)
         INSERT INTO mailbox_messages SELECT 'old-'||n,'mailbox',n,1,'sender','name','topic' FROM rows;
         INSERT INTO federation_mailbox_wake_obligations SELECT id,'{','pending' FROM mailbox_messages;"
    ).unwrap();
    connection.execute("INSERT INTO mailbox_messages VALUES('last','mailbox',1051,1,'sender','name','topic')", []).unwrap();
    connection.execute("INSERT INTO federation_mailbox_wake_obligations VALUES('last',?1,'pending')",
        [serde_json::to_string(&fixture.target).unwrap()]).unwrap();
    let mut cache = FederationExecutorReservationCache::default();
    assert!(cache.refresh(&connection).is_err(), "an incomplete scan cannot grant new capacity");
    assert_eq!(cache.scanned, 1000);
    let reserved = cache.refresh(&connection).unwrap();
    assert_eq!(reserved.len(), 1, "the excluded first page cannot poison the later valid wake");
    assert_eq!(reserved[0].1.wake.message_id, "last");
    assert_eq!(cache.unsupported, 1050);
    assert_eq!(cache.floor, 1050);
    assert!(cache.scanned <= 1000);
    let view = cache.summary();
    assert_eq!(view["windowOccupancy"], 1);
    assert!(!view.to_string().contains("payload"));
}

#[test]
fn federation_m4_review_stale_authority_is_excluded_but_link_loss_keeps_a_reservation() {
    let fixture = Fixture::new();
    fixture.central.refresh_federation_executor_reservation_domain().unwrap();
    assert_eq!(fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap().len(), 1);
    let image = fixture.central.mailbox_store.executor_reservations.lock().unwrap().domain.clone().unwrap();
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        inner.federation.links.clear();
    }
    fixture.central.refresh_federation_executor_reservation_domain().unwrap();
    assert!(fixture.central.mailbox_store.executor_reservations.lock().unwrap().domain.as_ref() == Some(&image));
    assert_eq!(fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap().len(), 1,
        "a disconnected peer is a wait condition, not a permanent exclusion");
    let peer = fixture.central.inner.lock().unwrap().federation.peers[
        &fixture.target.identity.host_id].clone();
    let earlier_generation = peer.enrollment_generation;
    fixture.central.enroll_federation_peer(peer).unwrap();
    let loaded = load_state(fixture.central.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(loaded.federation.peers[&fixture.target.identity.host_id].enrollment_generation,
        earlier_generation + 1, "the production enrollment transition must be durable");
    fixture.central.refresh_federation_executor_reservation_domain().unwrap();
    assert!(fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap().is_empty());
    assert!(fixture.central.mailbox_store.federation_executor_wake_state(
        fixture.request["wake"]["messageId"].as_str().unwrap()).unwrap().contains("stale_authority"));
}

#[test]
fn federation_m4_review_inflight_receipt_settles_after_a_rebuilt_prefix_fills_byte_capacity() {
    let fixture = Fixture::new();
    let request: FederationExecutorWakeRequest = serde_json::from_value(fixture.request.clone()).unwrap();
    let operation = fixture.operation();
    fixture.central.mailbox_store.reserve_exact_federation_executor_wake(&request, || Ok(())).unwrap();
    // S has already committed the original operation before C's view changes.
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    let mut pressure = operation.clone();
    pressure.operation_key = "pressure".into();
    pressure.payload = "x".repeat(FEDERATION_EXECUTOR_MAX_PAYLOAD);
    fixture.central.mailbox_store.admit_federation_executor_operation(&pressure, || Ok(())).unwrap();
    let rows = FEDERATION_EXECUTOR_MAX_BYTES / FEDERATION_EXECUTOR_MAX_PAYLOAD as u64;
    assert!(rows < FEDERATION_EXECUTOR_MAX_COUNT);
    fixture.central.mailbox_store.connection().unwrap().execute(
        "WITH RECURSIVE rows(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM rows WHERE n<?1)
         INSERT INTO federation_executor_operations(central_host_id,operation_key,central_epoch,
         target_host_id,enrollment_generation,session_id,creation_id,placement_generation,command_kind,
         payload_digest,payload,payload_bytes,stage,effect_state)
         SELECT central_host_id,'pressure-'||n,central_epoch,target_host_id,enrollment_generation,
         session_id,creation_id,placement_generation,command_kind,payload_digest,payload,payload_bytes,
         'received','not_started' FROM federation_executor_operations,rows WHERE operation_key='pressure'",
        [rows-1]).unwrap();
    assert!(fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap().is_empty());
    let receipt = FederationExecutorReceipt { protocol_version: FEDERATION_PROTOCOL_VERSION,
        operation: operation.clone(), payload_digest: operation.digest(), received: true };
    fixture.central.mailbox_store.settle_federation_executor_mirror(&request, &operation, &receipt, || Ok(())).unwrap();
    let bytes: u64 = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT SUM(payload_bytes) FROM federation_executor_operations", [], |row| row.get(0)).unwrap();
    assert!(bytes > FEDERATION_EXECUTOR_MAX_BYTES, "settlement cannot discard an already-owned operation");
    assert_eq!(fixture.inbox().unwrap().operation, operation);
    let mut later = operation;
    later.operation_key = "new-after-cap".into();
    assert!(fixture.central.mailbox_store.admit_federation_executor_operation(&later, || Ok(())).is_err(),
        "new admission waits while the old in-flight receipt settles");
}
