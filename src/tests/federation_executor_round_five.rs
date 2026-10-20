// Held production write-ahead callbacks and provider channels exercise host
// shutdown. SQLite readback proves late dormant writes, not an OS crash.
use super::*;
use super::round_two::idle_channel;
use super::round_four::no_external_runtime;

fn held_write_ahead(fixture: &mut Fixture) -> (Box<PersistFence>, String) {
    let writer = prompt_delivery::DeliveryWriter::attach(&mut fixture.satellite);
    assert!(fixture.satellite.dispatch_next_queued_turn(&fixture.native, false).unwrap().is_none());
    let fence = writer.take(prompt_delivery::HeldFence::WriteAhead);
    let nonce = {
        let inner = fixture.satellite.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        let delivery = &record.prompt_deliveries[&fixture.operation().queue_identity()];
        assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
        assert!(!delivery.write_ahead_acknowledged);
        assert!(delivery.turn_generation.is_none());
        persist_state_from_persisted(fixture.satellite.persistence_path.as_path(),
            &PersistedState::from_inner(&inner)).unwrap();
        delivery.write_ahead.clone()
    };
    let connection = rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    assert!(fence.target.is_already_durable(&connection).unwrap());
    drop(connection);
    // The actual fence remains unresolved. Closing this controlled writer
    // makes every later settlement use production synchronous persistence.
    drop(writer);
    (fence, nonce)
}

fn wait_for_settlement(fixture: &Fixture, nonce: &str) {
    let wait = phase_sync::PollGuard::new();
    loop {
        let settled = {
            let inner = fixture.satellite.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
            let delivery = &record.prompt_deliveries[&fixture.operation().queue_identity()];
            delivery.write_ahead != nonce
                || delivery.disposition != PromptDeliveryDisposition::HandingOff
        };
        if settled { return; }
        wait.wait("the exact shutdown handoff settles without sending");
    }
}

fn assert_durable_pending(fixture: &Fixture, old_nonce: &str) {
    let prompt = fixture.operation().queue_identity();
    let connection = rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    let json: String = connection.query_row("SELECT value_json FROM sessions WHERE id=?1",
        [&fixture.native], |row| row.get(0)).unwrap();
    let persisted: PersistedSessionRecord = serde_json::from_str(&json).unwrap();
    let record = persisted.into_record().unwrap();
    let delivery = &record.prompt_deliveries[&prompt];
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::Pending,
        "a positively unsent wake stays pending on disk after shutdown");
    assert_ne!(delivery.write_ahead, old_nonce);
    assert_eq!(record.queued_prompts.iter().filter(|entry| entry.pending_prompt.id == prompt).count(), 1);
    assert_eq!(delivery.federation_operation.as_ref(), Some(&fixture.operation()));
    let row = fixture.inbox().unwrap();
    assert_eq!(row.stage, "queued");
    assert_eq!(row.effect_state, "not_started");
    assert_eq!(row.delivery_attempt_id.as_deref(), Some(prompt.as_str()),
        "the durable logical queue identity is retained without an effect attempt");
    assert!(row.runtime_ack.is_none());
}

async fn stop_host(fixture: &Fixture) {
    let runtime = no_external_runtime(&fixture.satellite);
    fixture.satellite.trigger_shutdown_signal();
    runtime.shutdown().await;
    fixture.satellite.shutdown_persist_blocking();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_five_late_write_ahead_ok_stays_dormant_after_host_shutdown() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let input = idle_channel(&fixture, "late-write-ahead-ok");
    let (fence, nonce) = held_write_ahead(&mut fixture);
    stop_host(&fixture).await;
    fence.finish(Ok(()));
    wait_for_settlement(&fixture, &nonce);
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    assert_durable_pending(&fixture, &nonce);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_five_late_write_ahead_failure_persists_unsent_pending() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let input = idle_channel(&fixture, "late-write-ahead-failure");
    let (fence, nonce) = held_write_ahead(&mut fixture);
    stop_host(&fixture).await;
    fence.finish(Err(PersistFenceError::WorkerStopped));
    wait_for_settlement(&fixture, &nonce);
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    assert_durable_pending(&fixture, &nonce);
}

fn promote_after_readback(fixture: &Fixture, nonce: &str) -> TurnDispatch {
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let delivery = inner.sessions[index].prompt_deliveries.get_mut(&fixture.operation().queue_identity()).unwrap();
        assert_eq!(delivery.write_ahead, nonce);
        // A controlled acknowledgement marker after the actual SQLite check
        // above lets the ordinary promotion return its carrier to this test.
        // It does not claim any provider acknowledgement.
        delivery.write_ahead_acknowledged = true;
    }
    let dispatch = fixture.satellite.dispatch_next_queued_turn(&fixture.native, false).unwrap().unwrap();
    assert!(dispatch.federation_handoff().is_some());
    assert!(fixture.queue(&fixture.satellite).is_empty(), "ordinary promotion removes the queued head");
    dispatch
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_five_promoted_claude_carrier_is_not_consumed_by_host_shutdown() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let input = idle_channel(&fixture, "promoted-before-shutdown");
    let (fence, nonce) = held_write_ahead(&mut fixture);
    let dispatch = promote_after_readback(&fixture, &nonce);
    stop_host(&fixture).await;
    let outcome = deliver_turn_dispatch_now(&fixture.satellite, dispatch);
    assert!(matches!(outcome, TurnDispatchDeliveryOutcome::Held { .. }
        | TurnDispatchDeliveryOutcome::Superseded), "shutdown is a not-sent hold: {outcome:?}");
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    assert_durable_pending(&fixture, &nonce);
    drop(fence);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_five_fast_discovery_completion_after_shutdown_keeps_the_wake() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let (runtime, input) = test_codex_runtime_handle("late-fast-discovery");
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Codex(runtime);
        record.session.model = "catalog-model".into();
        record.session.codex_fast_mode = true;
        record.session.model_options.clear();
        record.session.status = SessionStatus::Idle;
        record.orchestrator_auto_dispatch_blocked = false;
        fixture.satellite.commit_locked(&mut inner).unwrap();
    }
    let (fence, nonce) = held_write_ahead(&mut fixture);
    let dispatch = promote_after_readback(&fixture, &nonce);
    assert!(matches!(&dispatch, TurnDispatch::PersistentCodex { service_tier: Err(_), .. }));
    assert!(matches!(deliver_turn_dispatch(&fixture.satellite, dispatch), TurnDispatchDeliveryOutcome::Scheduled));
    let CodexRuntimeCommand::RefreshModelList { response_tx } =
        phase_sync::receive(&input, "the real detached Fast worker holds catalog discovery")
    else { panic!("catalog discovery must precede the prompt"); };
    stop_host(&fixture).await;
    response_tx.send(Ok(codex_model_options(&json!({ "data": [{
        "model": "catalog-model", "serviceTiers": [{ "id": "Turbo-EXACT", "name": "Fast" }]
    }] })))).unwrap();
    wait_for_settlement(&fixture, &nonce);
    assert!(!matches!(input.try_recv(), Ok(CodexRuntimeCommand::Prompt { .. })));
    assert_durable_pending(&fixture, &nonce);
    drop(fence);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_five_boot_recovery_current_row_is_part_of_the_shutdown_join() {
    let mut fixture = Fixture::new();
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&fixture.operation(), || Ok(())).unwrap();
    let writer = super::round_three::QueueFenceWriter::attach(&mut fixture.satellite);
    let signal = &fixture.satellite.federation_executor_wake_signal;
    // An empty join can itself first poll Pending while spawn_blocking is
    // scheduled. Await that negative control; it proves no registered owner.
    assert_eq!(*signal.receives.lock().unwrap(), 0);
    signal.join_receives().await;
    let app = fixture.satellite.clone();
    // Call the exact M4 step used by the unrelated post-listen boot owner;
    // this test never starts that owner's Engram CLI recovery.
    let boot = std::thread::spawn(move || app.recover_federation_executor_after_boot());
    let fence = phase_sync::receive(&writer.held, "boot recovery owns its current queue row");
    let registered = *signal.receives.lock().unwrap();
    let serialized = matches!(signal.recovery_pass.try_lock(),
        Err(std::sync::TryLockError::WouldBlock));
    signal.stop();
    let mut join = Box::pin(signal.join_receives());
    let was_pending = std::future::poll_fn(|context| {
        std::task::Poll::Ready(matches!(std::future::Future::poll(join.as_mut(), context),
            std::task::Poll::Pending))
    }).await;
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        persist_state_from_persisted(fixture.satellite.persistence_path.as_path(),
            &PersistedState::from_inner(&inner)).unwrap();
    }
    let connection = rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    assert!(fence.target.is_already_durable(&connection).unwrap());
    drop(connection);
    fence.finish(Ok(()));
    tokio::task::spawn_blocking(move || boot.join().unwrap()).await.unwrap();
    if was_pending { join.await; }
    assert_eq!(registered, 1, "the held boot step must actually register, independently of join scheduling");
    assert!(serialized, "boot and supervisor recovery share the held pass mutex");
    assert!(was_pending, "the M4 boot row must be owned until its queue association finishes");
    assert_eq!(*signal.receives.lock().unwrap(), 0);
    assert_eq!(fixture.inbox().unwrap().stage, "queued");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_five_codex_command_queued_before_shutdown_cannot_begin_runtime_write() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let (runtime, input) = test_codex_runtime_handle("shutdown-after-codex-enqueue");
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Codex(runtime);
        record.session.codex_fast_mode = false;
        record.session.status = SessionStatus::Idle;
        record.orchestrator_auto_dispatch_blocked = false;
        fixture.satellite.commit_locked(&mut inner).unwrap();
    }
    let (fence, nonce) = held_write_ahead(&mut fixture);
    let dispatch = promote_after_readback(&fixture, &nonce);
    let token = dispatch.runtime_token().clone();
    let turn = dispatch.active_turn_generation();
    assert!(matches!(handoff_prepared_turn_dispatch(&fixture.satellite, dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered));
    let CodexRuntimeCommand::Prompt { session_id, command } =
        phase_sync::receive(&input, "the actual Codex prompt command is queued before its write")
    else { panic!("expected the receipted Codex prompt"); };
    assert!(command.delivery_receipt.is_some());
    assert_eq!(fixture.inbox().unwrap().effect_state, "unknown");
    stop_host(&fixture).await;
    assert!(!fixture.satellite.begin_prompt_delivery_runtime_write(&session_id, &token,
        turn, command.delivery_receipt.as_ref()), "host shutdown fences the actual prompt-carrying write");
    assert_eq!(fixture.inbox().unwrap().effect_state, "unknown",
        "a denied runtime write never regresses the durable effect evidence");
    let inner = fixture.satellite.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
    let receipt = &record.prompt_deliveries[&fixture.operation().queue_identity()];
    assert_eq!(receipt.runtime_write, PromptRuntimeWrite::Pending);
    assert_eq!(receipt.write_ahead, nonce);
    assert!(record.queued_prompts.is_empty());
    drop(inner);
    drop(fence);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_five_unsent_carrier_never_replays_a_durable_unknown_effect() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let input = idle_channel(&fixture, "shutdown-after-effect-mark");
    let (fence, nonce) = held_write_ahead(&mut fixture);
    let dispatch = promote_after_readback(&fixture, &nonce);
    fixture.satellite.prepare_federation_executor_handoff(&fixture.native,
        dispatch.runtime_token(), dispatch.active_turn_generation(), dispatch.federation_handoff()).unwrap().unwrap();
    assert_eq!(fixture.inbox().unwrap().effect_state, "unknown");
    stop_host(&fixture).await;
    assert!(matches!(deliver_turn_dispatch_now(&fixture.satellite, dispatch),
        TurnDispatchDeliveryOutcome::Held { .. }));
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    assert_eq!(fixture.inbox().unwrap().effect_state, "unknown");
    let inner = fixture.satellite.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
    let receipt = &record.prompt_deliveries[&fixture.operation().queue_identity()];
    assert_eq!(receipt.disposition, PromptDeliveryDisposition::Ambiguous);
    assert_eq!(receipt.write_ahead, nonce);
    assert!(record.queued_prompts.is_empty());
    drop(inner);
    drop(fence);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_five_shutdown_restored_carrier_recovers_its_exact_content_hold() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let operation = fixture.operation();
    let prompt = operation.queue_identity();
    let original = fixture.queue(&fixture.satellite)[0].clone();
    let original_hold_nonce = {
        let inner = fixture.satellite.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.native).unwrap()]
            .queued_prompt_durability_holds[&prompt].nonce.clone()
    };
    let input = idle_channel(&fixture, "restored-content-before-reload");
    let (fence, nonce) = held_write_ahead(&mut fixture);
    let dispatch = promote_after_readback(&fixture, &nonce);
    stop_host(&fixture).await;
    assert!(matches!(deliver_turn_dispatch_now(&fixture.satellite, dispatch),
        TurnDispatchDeliveryOutcome::Held { .. }));
    drop(fence);
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    assert_durable_pending(&fixture, &nonce);
    let restored_hold = {
        let inner = fixture.satellite.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.native).unwrap()]
            .queued_prompt_durability_holds.get(&prompt).cloned()
    };
    let persisted = {
        let connection = rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
        let json: String = connection.query_row("SELECT value_json FROM sessions WHERE id=?1",
            [&fixture.native], |row| row.get(0)).unwrap();
        serde_json::from_str::<PersistedSessionRecord>(&json).unwrap()
    };
    // The actual saved row and real boot conversion, not a fabricated hold.
    let reloaded = fixture.reload();
    let (runtime, resumed) = test_claude_runtime_handle("restored-content-after-reload");
    {
        let mut inner = reloaded.0.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Claude(runtime);
        record.session.status = SessionStatus::Idle;
        reloaded.0.commit_locked(&mut inner).unwrap();
    }
    let result = reloaded.0.queue_federation_executor_operation(&operation, false);
    assert!(result.is_ok(), "restored Pending association must be recoverable: {result:?}");
    FederationExecutorRecovery::default().recover(&reloaded.0);
    let restored_hold = restored_hold.expect("shutdown must restore standalone content protection");
    assert!(!restored_hold.acknowledged, "shutdown cannot invent a persistence acknowledgement");
    assert_ne!(restored_hold.nonce, original_hold_nonce);
    assert_eq!(persisted.queued_prompt_durability_holds.get(&prompt), Some(&restored_hold),
        "the exact fresh hold and restored content commit in the same session row");
    // Snapshot before assertions: a failed assertion must not poison the
    // mutex and panic again while the real reload owner joins its writer.
    let (queue, holds, paused, disposition, standalone) = {
        let inner = reloaded.0.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &inner.sessions[index];
        (record.queued_prompts.clone(), record.queued_prompt_durability_holds.clone(),
            record.orchestrator_auto_dispatch_blocked, record.prompt_deliveries.get(&prompt).map(|receipt| receipt.disposition),
            record.queued_prompts.front().is_some_and(|entry| queued_prompt_kept_standalone(record, entry)))
    };
    assert_eq!(queue.len(), 1);
    let entry = queue.front().unwrap();
    assert_eq!(entry.pending_prompt.id, prompt);
    assert_eq!(entry.pending_prompt.text, original.pending_prompt.text);
    assert_eq!(entry.pending_prompt.source, original.pending_prompt.source);
    assert!(entry.promoted_message_index.is_some(), "the existing transcript message is retained");
    let hold = &holds[&prompt];
    assert!(hold.acknowledged, "real persisted-row load acknowledges the exact saved hold");
    assert_eq!(hold.nonce, restored_hold.nonce, "recovery retains the saved hold identity");
    assert_eq!(hold.content, queued_prompt_durability_content(entry));
    assert!(standalone);
    assert_eq!(disposition, Some(PromptDeliveryDisposition::Pending));
    assert!(!paused, "recovery preserves the unpaused flag; dormancy does not manufacture a pause");
    assert!(matches!(resumed.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "recovery without a same-boot live activation sends no prompt");
    // Separate explicit-pause fixture control. The actual Resume below must
    // release a paused head; it is not an activation of the dormant boot pass.
    {
        let mut inner = reloaded.0.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
        reloaded.0.commit_locked(&mut inner).unwrap();
    }
    assert!(reloaded.0.dispatch_next_queued_turn(&fixture.native, false).unwrap().is_none());
    assert!(matches!(resumed.try_recv(), Err(mpsc::TryRecvError::Empty)));
    reloaded.0.resume_local_session_queue(&fixture.native, None).unwrap();
    assert!(matches!(phase_sync::receive(&resumed, "explicit Resume releases the restored exact wake"),
        ClaudeRuntimeCommand::Prompt(_)));
    assert!(!matches!(resumed.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    assert!(fixture.queue(&reloaded.0).is_empty());
}
