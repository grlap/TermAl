// Production delivery and receive-budget regressions. Explicit temporary
// authority flags below are fault injection, not an OS crash or a new lease.
use super::*;
use super::round_two::idle_channel;

fn restore_durable(fixture: &Fixture) {
    fixture.satellite.inner.lock().unwrap().federation_durable = true;
}

fn has_live_intent(fixture: &Fixture, operation: &FederationExecutorOperation) -> bool {
    fixture.satellite.federation_executor_wake_signal.live.lock().unwrap()
        .contains_key(&AppState::federation_executor_live_key(operation))
}

fn finish_recovered_handoff(fixture: &mut Fixture, input: &mpsc::Receiver<ClaudeRuntimeCommand>) {
    let writer = prompt_delivery::DeliveryWriter::attach(&mut fixture.satellite);
    FederationExecutorRecovery::default().recover(&fixture.satellite);
    let fence = writer.take(prompt_delivery::HeldFence::WriteAhead);
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        persist_state_from_persisted(fixture.satellite.persistence_path.as_path(),
            &PersistedState::from_inner(&inner)).unwrap();
    }
    let connection = rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    assert!(fence.target.is_already_durable(&connection).unwrap());
    drop(connection);
    fence.finish(Ok(()));
    assert!(matches!(phase_sync::receive(input, "recovered activation after authority hold"), ClaudeRuntimeCommand::Prompt(_)));
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    let record = fixture.inbox().unwrap();
    assert_eq!(record.effect_state, "unknown");
    assert!(record.queue_entry_id.is_some());
    assert!(record.runtime_ack.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_three_transient_hold_before_live_capture_keeps_activation() {
    let mut fixture = Fixture::new();
    let operation = fixture.operation();
    let input = idle_channel(&fixture, "before-capture-hold");
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    // Model the existing receive-COMMIT -> queue seam while an unrelated
    // binding's persistence temporarily withholds federation durability.
    fixture.satellite.inner.lock().unwrap().federation_durable = false;
    assert!(fixture.satellite.queue_federation_executor_operation(&operation, true).is_err());
    let retained = has_live_intent(&fixture, &operation);
    restore_durable(&fixture);
    assert!(retained, "a transient hold must not lose the already-received same-boot activation");
    finish_recovered_handoff(&mut fixture, &input);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_three_transient_hold_after_queue_ack_keeps_activation() {
    let mut fixture = Fixture::new();
    let operation = fixture.operation();
    let input = idle_channel(&fixture, "after-queue-ack-hold");
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    FEDERATION_EXECUTOR_TEST_CUT.with(|slot| *slot.borrow_mut() = Some("afterQueueDurabilityAck"));
    assert!(fixture.satellite.queue_federation_executor_operation(&operation, true).is_err());
    assert!(FEDERATION_EXECUTOR_TEST_CUT.with(|slot| slot.borrow().is_none()));
    assert!(has_live_intent(&fixture, &operation));
    fixture.satellite.inner.lock().unwrap().federation_durable = false;
    fixture.satellite.dispatch_federation_executor_live_activation(&operation);
    let retained = has_live_intent(&fixture, &operation);
    restore_durable(&fixture);
    assert!(retained, "temporary authority unavailability is not permanent retirement");
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
    finish_recovered_handoff(&mut fixture, &input);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_three_outer_settlement_cannot_reset_a_new_nonce() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let (token, sender, input) = prepare_channel(&fixture, "outer-settlement", 1);
    let carrier = channel_dispatch(&fixture, &token, &sender, 1);
    let fresh = Uuid::new_v4().to_string();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].prompt_deliveries.get_mut(&fixture.operation().queue_identity()).unwrap()
            .write_ahead = fresh.clone();
        fixture.satellite.commit_locked(&mut inner).unwrap();
    }
    assert!(matches!(deliver_turn_dispatch_now(&fixture.satellite, carrier), TurnDispatchDeliveryOutcome::Superseded));
    let (status, disposition, nonce) = {
        let inner = fixture.satellite.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        let receipt = &record.prompt_deliveries[&fixture.operation().queue_identity()];
        (record.session.status, receipt.disposition, receipt.write_ahead.clone())
    };
    assert_eq!(status, SessionStatus::Active);
    assert_eq!(disposition, PromptDeliveryDisposition::HandingOff, "outer settlement must preserve the successor");
    assert_eq!(nonce, fresh);
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
}

pub(super) struct QueueFenceWriter {
    tx: mpsc::Sender<PersistRequest>,
    pub(super) held: mpsc::Receiver<Box<PersistFence>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
}

impl QueueFenceWriter {
    pub(super) fn attach(state: &mut AppState) -> Self {
        let (tx, rx) = mpsc::channel();
        state.persist_tx = tx.clone();
        let (held_tx, held) = mpsc::channel();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = stop.clone();
        let task = std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                if stopping.load(Ordering::SeqCst) { break; }
                if let PersistRequest::Fence(fence) = request {
                    if matches!(&fence.target, PersistFenceTarget::FederationExecutorQueue { .. }) {
                        let _ = held_tx.send(fence);
                    } else {
                        fence.finish(Err(PersistFenceError::WriteFailed("fixture holds persistence".into())));
                    }
                }
            }
        });
        Self { tx, held, stop, task: Some(task) }
    }
}

impl Drop for QueueFenceWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(PersistRequest::Delta);
        if let Some(task) = self.task.take() { task.join().unwrap(); }
        while let Ok(fence) = self.held.try_recv() {
            fence.finish(Err(PersistFenceError::WriteFailed("fixture released held persistence".into())));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_three_received_receipt_does_not_wait_for_queue_fence() {
    let mut fixture = Fixture::new();
    let writer = QueueFenceWriter::attach(&mut fixture.satellite);
    // for_app captured an AppState value before the controlled writer was
    // attached. Preserve the attested channel/basis/security, replacing only
    // its application clone so the HTTP handler uses this same persist sender.
    fixture.channel.app = Some(fixture.satellite.clone());
    // The production five-second listener deadline remains unchanged. The
    // fixture does not release the queue fence before the response is read.
    let response = fixture.receive_raw(&fixture.request).await;
    let journal = fixture.inbox().unwrap();
    let link_current = fixture.channel.current_lease().is_ok();
    let held = writer.held.try_recv().ok();
    let intercepted = held.is_some();
    if let Some(fence) = held {
        fence.finish(Err(PersistFenceError::WriteFailed("fixture released observed queue fence".into())));
    }
    drop(writer);
    assert_eq!(response.0, StatusCode::ACCEPTED,
        "receive COMMIT must answer while local queue persistence is held; intercepted={intercepted}, stage={}, link_current={link_current}", journal.stage);
    assert_eq!(journal.stage, "received");
    assert!(journal.runtime_ack.is_none());
    assert!(link_current, "a held queue fence must not fence the authenticated pair");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_three_duplicate_receive_does_not_create_another_operation() {
    let fixture = Fixture::new();
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    assert_eq!(fixture.receive(&fixture.request).await.0, StatusCode::ACCEPTED);
    let count: u64 = fixture.satellite.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_executor_operations", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 1);
    assert_eq!(fixture.inbox().unwrap().effect_state, "not_started");
}

#[test]
fn federation_m4_round_three_failed_receive_commit_has_no_live_activation() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    FEDERATION_EXECUTOR_TEST_CUT.with(|slot| *slot.borrow_mut() = Some("beforeReceiveCommit"));
    assert!(fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).is_err());
    assert!(FEDERATION_EXECUTOR_TEST_CUT.with(|slot| slot.borrow().is_none()));
    assert!(fixture.inbox().is_none());
    assert!(!has_live_intent(&fixture, &operation));
}

#[test]
fn federation_m4_round_three_completed_stop_cannot_reactivate_an_older_intent() {
    let fixture = Fixture::new();
    let operation = fixture.operation();
    let input = idle_channel(&fixture, "stop-generation");
    fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    FEDERATION_EXECUTOR_TEST_CUT.with(|slot| *slot.borrow_mut() = Some("afterQueueDurabilityAck"));
    assert!(fixture.satellite.queue_federation_executor_operation(&operation, true).is_err());
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let record = &mut inner.sessions[index];
        // Production Stop ownership transition; this is not a claimed OS stop.
        record.claim_missing_runtime_stop(RuntimeStopOwnerKind::LostRuntimeTerminalization);
        record.clear_runtime_stop();
    }
    fixture.satellite.dispatch_federation_executor_live_activation(&operation);
    assert!(!has_live_intent(&fixture, &operation));
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_three_owned_worker_keeps_one_credit_until_queue_is_durable() {
    let mut fixture = Fixture::new();
    let writer = QueueFenceWriter::attach(&mut fixture.satellite);
    fixture.channel.app = Some(fixture.satellite.clone());
    let capacity = fixture.channel.bound.permits.available_permits();
    assert_eq!(fixture.receive_raw(&fixture.request).await.0, StatusCode::ACCEPTED);
    assert_eq!(fixture.inbox().unwrap().stage, "received");
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity - 1,
        "the HTTP middleware dropped its Arc, leaving the task's one credit");
    assert_eq!(fixture.receive_raw(&fixture.request).await.0, StatusCode::ACCEPTED);
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity - 1);
    assert_eq!(fixture.satellite.federation_executor_wake_signal.activation_work.lock().unwrap().len(), 1);
    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    let fence = phase_sync::receive(&writer.held, "owned activation queue fence");
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity - 1);
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        persist_state_from_persisted(fixture.satellite.persistence_path.as_path(),
            &PersistedState::from_inner(&inner)).unwrap();
    }
    let connection = rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    assert!(fence.target.is_already_durable(&connection).unwrap());
    drop(connection);
    fence.finish(Ok(()));
    worker.shutdown(&fixture.satellite).await;
    assert_eq!(fixture.inbox().unwrap().stage, "queued");
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity);
    assert!(fixture.satellite.federation_executor_wake_signal.activation_work.lock().unwrap().is_empty());
}

#[test]
fn federation_m4_round_three_failed_commit_never_calls_capture_or_keeps_credit() {
    let fixture = Fixture::new();
    let capacity = fixture.channel.bound.permits.available_permits();
    let permit = Arc::new(fixture.channel.bound.try_acquire().unwrap());
    let captured = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = captured.clone();
    FEDERATION_EXECUTOR_TEST_CUT.with(|slot| *slot.borrow_mut() = Some("beforeReceiveCommit"));
    let result = fixture.satellite.mailbox_store.admit_federation_executor_operation_after_commit(
        &fixture.operation(), || Ok(()), move |_, _| {
            observed.store(true, Ordering::SeqCst);
            drop(permit);
        });
    assert!(result.is_err());
    assert!(!captured.load(Ordering::SeqCst));
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity);
    assert!(fixture.inbox().is_none());
    assert!(fixture.satellite.federation_executor_wake_signal.activation_work.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_three_terminal_identity_refusal_releases_owned_credit() {
    let fixture = Fixture::new();
    let capacity = fixture.channel.bound.permits.available_permits();
    assert_eq!(fixture.receive_raw(&fixture.request).await.0, StatusCode::ACCEPTED);
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity - 1);
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        // Explicit permanent captured-binding retirement, not a fresh lease.
        inner.sessions[index].federation_mailbox_binding = None;
    }
    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    worker.shutdown(&fixture.satellite).await;
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity);
    assert_eq!(fixture.inbox().unwrap().stage, "received");
    assert!(fixture.queue(&fixture.satellite).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_three_cancelled_receive_waiter_keeps_its_owned_commit_credit() {
    let fixture = Fixture::new();
    let capacity = fixture.channel.bound.permits.available_permits();
    let permit = Arc::new(fixture.channel.bound.try_acquire().unwrap());
    let app = fixture.satellite.clone();
    let operation = fixture.operation();
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (committed_tx, committed) = mpsc::channel();
    // This is the production owned blocking receive seam, not an HTTP ACK.
    // Cancel only its waiter while the actual authority guard holds COMMIT.
    let waiter = tokio::spawn(async move {
        run_blocking_api(move || {
            let authority = FederationExecutorAuthority { app: &app, operation: &operation, lease: None };
            app.mailbox_store.admit_federation_executor_operation_after_commit(&operation, || {
                let authority = authority.lock()?;
                let live = app.federation_executor_wake_signal.live.lock().unwrap();
                let work = app.federation_executor_wake_signal.activation_work.lock().unwrap();
                entered_tx.send(()).unwrap();
                phase_sync::receive(&release_rx, "release cancelled receive COMMIT");
                Ok(FederationExecutorReceiveGuard { authority, live, work })
            }, |guard, record| {
                app.capture_federation_executor_receive(guard, record, permit);
                committed_tx.send(()).unwrap();
            }).map_err(mailbox_api_error)
        }).await
    });
    phase_sync::receive(&entered, "owned receive entered authority guard");
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity - 1);
    release.send(()).unwrap();
    phase_sync::receive(&committed, "cancelled waiter worker committed once");
    assert_eq!(fixture.inbox().unwrap().stage, "received");
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity - 1);
    // A lost response can safely retry the identical application key.
    assert_eq!(fixture.receive_raw(&fixture.request).await.0, StatusCode::ACCEPTED);
    assert_eq!(fixture.satellite.federation_executor_wake_signal.activation_work.lock().unwrap().len(), 1);
    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    worker.shutdown(&fixture.satellite).await;
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity);
    assert_eq!(fixture.inbox().unwrap().stage, "queued");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_three_authenticated_capture_survives_a_temporary_hold() {
    let mut fixture = Fixture::new();
    let operation = fixture.operation();
    let input = idle_channel(&fixture, "captured-live-receive");
    let capacity = fixture.channel.bound.permits.available_permits();
    assert_eq!(fixture.receive_raw(&fixture.request).await.0, StatusCode::ACCEPTED);
    assert!(has_live_intent(&fixture, &operation));
    fixture.satellite.inner.lock().unwrap().federation_durable = false;
    let blocked = fixture.satellite.clone();
    assert!(!run_blocking_api(move || Ok(blocked.run_federation_executor_activation_batch())).await.unwrap());
    assert!(has_live_intent(&fixture, &operation));
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity - 1);
    restore_durable(&fixture);
    finish_recovered_handoff(&mut fixture, &input);
    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    worker.shutdown(&fixture.satellite).await;
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity);
    assert!(!matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))));
}
