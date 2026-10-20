// A controlled queue-content fence, production cancellation and actual SQLite
// materialization. These are not an OS crash or a provider acknowledgement.
use super::*;
use super::round_three::QueueFenceWriter;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_cancelled_association_releases_credit_and_later_work_progresses() {
    let mut fixture = Fixture::new();
    append_wake(&fixture, "Second sender", "second retained operation");
    fixture.central.refresh_federation_executor_reservation_domain().unwrap();
    let mut requests = fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap();
    assert_eq!(requests.len(), 2);
    requests.sort_by(|left, right| left.operation_key.cmp(&right.operation_key));
    let operations: Vec<_> = requests.iter().map(|request|
        request.operation(fixture.channel.snapshot.enrollment_generation).unwrap()).collect();
    let writer = QueueFenceWriter::attach(&mut fixture.satellite);
    fixture.channel.app = Some(fixture.satellite.clone());
    let capacity = fixture.channel.bound.permits.available_permits();
    for request in &requests {
        assert_eq!(fixture.receive_raw(&serde_json::to_value(request).unwrap()).await.0, StatusCode::ACCEPTED);
    }
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity - 2);
    let association = fixture.satellite.clone();
    let first = operations[0].clone();
    let attempt = tokio::spawn(async move {
        run_blocking_api(move || association.queue_federation_executor_operation(&first, false)).await
    });
    let fence = phase_sync::receive(&writer.held, "first operation held before queue durability");
    fixture.satellite.cancel_queued_prompt(&fixture.native, &operations[0].queue_identity()).unwrap();
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        assert!(!record.queued_prompts.iter().any(|entry|
            entry.pending_prompt.id == operations[0].queue_identity()));
        assert!(record.prompt_deliveries.contains_key(&operations[0].queue_identity()),
            "cancellation retains the exact federation receipt tombstone");
        persist_state_from_persisted(fixture.satellite.persistence_path.as_path(),
            &PersistedState::from_inner(&inner)).unwrap();
    }
    let connection = rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    assert!(!fence.target.is_already_durable(&connection).unwrap(),
        "the canceled queue content was never acknowledged as durable");
    drop(connection);
    fence.finish(Err(PersistFenceError::WriteFailed("controlled queue cancellation".into())));
    assert!(attempt.await.unwrap().is_err());
    drop(writer); // Restores the existing disconnected synchronous fallback.
    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    // Join the exact batch without shutdown clearing pending work/credits.
    worker.task.take().unwrap().await.unwrap();
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity,
        "terminal local cancellation must release its credit and let the second association finish");
    let rows: Vec<_> = operations.iter().map(|operation|
        federation_executor_read(&fixture.satellite.mailbox_store.connection().unwrap(),
            &operation.central_host_id, &operation.operation_key).unwrap().unwrap()).collect();
    assert_eq!(rows[0].stage, "received");
    assert_eq!(rows[0].effect_state, "not_started");
    assert!(rows[0].queue_entry_id.is_none());
    assert_eq!(rows[1].stage, "queued");
    assert_eq!(rows[1].effect_state, "not_started");
    assert_eq!(fixture.queue(&fixture.satellite).len(), 1);
    assert!(fixture.satellite.federation_executor_wake_signal.activation_work.lock().unwrap().is_empty());
    worker.shutdown(&fixture.satellite).await;
}

fn two_received_wakes(fixture: &Fixture) -> Vec<FederationExecutorWakeRequest> {
    append_wake(fixture, "Second sender", "second retained operation");
    fixture.central.refresh_federation_executor_reservation_domain().unwrap();
    let mut requests = fixture.central.mailbox_store.reserve_federation_executor_wakes().unwrap();
    requests.sort_by(|left, right| left.operation_key.cmp(&right.operation_key));
    assert_eq!(requests.len(), 2);
    requests
}

async fn assert_join_pending<F: std::future::Future>(mut join: std::pin::Pin<&mut F>) {
    std::future::poll_fn(|context| {
        assert!(matches!(std::future::Future::poll(join.as_mut(), context), std::task::Poll::Pending),
            "an owned join cannot complete while its controlled current unit is held");
        std::task::Poll::Ready(())
    }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_transient_first_key_does_not_starve_the_second_or_spin() {
    let fixture = Fixture::new();
    let requests = two_received_wakes(&fixture);
    for request in &requests {
        assert_eq!(fixture.receive_raw(&serde_json::to_value(request).unwrap()).await.0, StatusCode::ACCEPTED);
    }
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = count.clone();
    let state = fixture.satellite.clone();
    *fixture.satellite.federation_executor_wake_signal.lifecycle_hook.lock().unwrap() = Some(Arc::new(move |event| {
        if event == "activationClaimed" {
            let attempt = seen.fetch_add(1, Ordering::SeqCst);
            // A controlled temporary global durability hold affects key 1;
            // clear it before key 2, without retiring either identity.
            state.inner.lock().unwrap().federation_durable = attempt != 0;
        }
    }));
    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    worker.task.take().unwrap().await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2, "one attempt per key, no retry spin");
    let signal = &fixture.satellite.federation_executor_wake_signal;
    let work = signal.activation_work.lock().unwrap();
    assert_eq!(work.len(), 1);
    assert!(work.values().all(|entry| !entry.in_flight && entry.permit.is_some()));
    assert_eq!(work.keys().next().unwrap().1, requests[0].operation_key);
    drop(work);
    assert_eq!(fixture.queue(&fixture.satellite).len(), 1);
    *signal.lifecycle_hook.lock().unwrap() = None;
    worker.shutdown(&fixture.satellite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_unwound_claim_restores_its_one_credit_for_retry() {
    let fixture = Fixture::new();
    let capacity = fixture.channel.bound.permits.available_permits();
    assert_eq!(fixture.receive_raw(&fixture.request).await.0, StatusCode::ACCEPTED);
    let signal = &fixture.satellite.federation_executor_wake_signal;
    *signal.lifecycle_hook.lock().unwrap() = Some(Arc::new(|event| {
        if event == "activationClaimed" { panic!("controlled activation claim unwind"); }
    }));
    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    worker.task.take().unwrap().await.unwrap();
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity - 1);
    let work = signal.activation_work.lock().unwrap();
    assert_eq!(work.len(), 1);
    assert!(work.values().all(|entry| !entry.in_flight && entry.permit.is_some()));
    drop(work);
    *signal.lifecycle_hook.lock().unwrap() = None;
    worker.tick(&fixture.satellite, true).await;
    worker.task.take().unwrap().await.unwrap();
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity);
    assert_eq!(fixture.inbox().unwrap().stage, "queued");
    worker.shutdown(&fixture.satellite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_stopping_refuses_receive_before_writer_and_all_kicks() {
    let fixture = Fixture::new();
    let signal = &fixture.satellite.federation_executor_wake_signal;
    let capacity = fixture.channel.bound.permits.available_permits();
    let generation = signal.generation.load(Ordering::SeqCst);
    signal.stop();
    signal.kick();
    assert_eq!(signal.generation.load(Ordering::SeqCst), generation);
    assert_eq!(fixture.receive_raw(&fixture.request).await.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(fixture.inbox().is_none());
    assert_eq!(fixture.channel.bound.permits.available_permits(), capacity);
    assert_eq!(*signal.receives.lock().unwrap(), 0);
    assert!(signal.activation_work.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_stopping_worker_finishes_only_its_current_association() {
    let mut fixture = Fixture::new();
    let requests = two_received_wakes(&fixture);
    let writer = QueueFenceWriter::attach(&mut fixture.satellite);
    fixture.channel.app = Some(fixture.satellite.clone());
    for request in &requests {
        assert_eq!(fixture.receive_raw(&serde_json::to_value(request).unwrap()).await.0, StatusCode::ACCEPTED);
    }
    let mut worker = FederationExecutorActivationWorker::default();
    worker.tick(&fixture.satellite, true).await;
    let fence = phase_sync::receive(&writer.held, "activation current durable step");
    fixture.satellite.federation_executor_wake_signal.stop();
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        persist_state_from_persisted(fixture.satellite.persistence_path.as_path(), &PersistedState::from_inner(&inner)).unwrap();
    }
    let connection = rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    assert!(fence.target.is_already_durable(&connection).unwrap());
    drop(connection);
    fence.finish(Ok(()));
    worker.shutdown(&fixture.satellite).await;
    assert_eq!(fixture.queue(&fixture.satellite).len(), 1, "the second unit is not admitted");
    let second = requests[1].operation(fixture.channel.snapshot.enrollment_generation).unwrap();
    assert_eq!(federation_executor_read(&fixture.satellite.mailbox_store.connection().unwrap(),
        &second.central_host_id, &second.operation_key).unwrap().unwrap().stage, "received");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_receiver_recovery_is_joined_and_stops_between_rows() {
    let mut fixture = Fixture::new();
    for request in two_received_wakes(&fixture) {
        let operation = request.operation(fixture.channel.snapshot.enrollment_generation).unwrap();
        fixture.satellite.mailbox_store.admit_federation_executor_operation(&operation, || Ok(())).unwrap();
    }
    let writer = QueueFenceWriter::attach(&mut fixture.satellite);
    let mut recovery = FederationExecutorRecovery::default();
    recovery.tick(&fixture.satellite).await;
    let fence = phase_sync::receive(&writer.held, "receiver recovery current durable step");
    fixture.satellite.federation_executor_wake_signal.stop();
    let mut join = Box::pin(recovery.shutdown());
    assert_join_pending(join.as_mut()).await;
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        persist_state_from_persisted(fixture.satellite.persistence_path.as_path(),
            &PersistedState::from_inner(&inner)).unwrap();
    }
    let connection = rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    assert!(fence.target.is_already_durable(&connection).unwrap());
    drop(connection);
    fence.finish(Ok(()));
    join.await;
    assert_eq!(fixture.queue(&fixture.satellite).len(), 1, "recovery did not begin a second row");
}

pub(super) fn no_external_runtime(app: &AppState) -> FederationRuntime {
    let mut io = FederationRuntimeIo::default();
    io.credentials = Arc::new(|_| Err(ApiError::from_status(StatusCode::SERVICE_UNAVAILABLE, "fixture has no external credentials")));
    io.ssh = Arc::new(|_, _, _| panic!("no SSH in lifecycle fixture"));
    start_federation_runtime_with_io(app.clone(), io)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_host_signal_fences_receive_before_supervisor_joins() {
    let fixture = Fixture::new();
    let signal = &fixture.satellite.federation_executor_wake_signal;
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    *signal.lifecycle_hook.lock().unwrap() = Some(Arc::new(move |event| {
        observed.lock().unwrap().push(event);
    }));
    let runtime = no_external_runtime(&fixture.satellite);
    fixture.satellite.trigger_shutdown_signal();
    assert!(signal.is_stopping(), "the host signal fences executor admission synchronously");
    assert_eq!(fixture.receive_raw(&fixture.request).await.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(fixture.inbox().is_none());
    runtime.shutdown().await;
    fixture.satellite.shutdown_persist_blocking();
    let recorded = events.lock().unwrap().clone();
    let stopped = recorded.iter().position(|event| *event == "stopping").unwrap();
    let joined = recorded.iter().position(|event| *event == "supervisorJoined").unwrap();
    let persisted = recorded.iter().position(|event| *event == "persistShutdown").unwrap();
    assert!(stopped < joined && joined < persisted, "{recorded:?}");
    *signal.lifecycle_hook.lock().unwrap() = None;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_receive_waiting_for_writer_refuses_after_stop() {
    let fixture = Arc::new(Fixture::new());
    let signal = &fixture.satellite.federation_executor_wake_signal;
    let (entered_tx, entered) = mpsc::channel();
    *signal.lifecycle_hook.lock().unwrap() = Some(Arc::new(move |event| {
        if event == "receiveWriterAdmission" { entered_tx.send(()).unwrap(); }
    }));
    let writer = fixture.satellite.mailbox_store.lock_writer("controlled receive writer admission").unwrap();
    let receiver = fixture.clone();
    let response = tokio::spawn(async move { receiver.receive_raw(&receiver.request).await });
    phase_sync::receive(&entered, "receive waiting for writer");
    signal.stop();
    drop(writer);
    let response = response.await.unwrap();
    assert_eq!(response.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(fixture.inbox().is_none());
    assert_eq!(*signal.receives.lock().unwrap(), 0);
    *signal.lifecycle_hook.lock().unwrap() = None;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_sender_recovery_current_row_is_owned_until_join() {
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
    let signal = &fixture.central.federation_executor_wake_signal;
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    *signal.lifecycle_hook.lock().unwrap() = Some(Arc::new(move |event| {
        if event == "senderRecoveryRow" {
            entered_tx.send(()).unwrap();
            phase_sync::receive(&released.lock().unwrap(), "release owned sender row");
        }
    }));
    let mut recovery = FederationExecutorSenderRecovery::default();
    recovery.tick(&fixture.central).await;
    phase_sync::receive(&entered, "sender recovery owns its current row");
    signal.stop();
    let mut join = Box::pin(recovery.shutdown());
    assert_join_pending(join.as_mut()).await;
    release.send(()).unwrap();
    join.await;
    assert_eq!(fixture.central.local_federation_intent(&sender, &delivery.key).unwrap().state, "homeCommitted");
    let mut stopped = FederationExecutorSenderRecovery::default();
    stopped.tick(&fixture.central).await;
    assert!(stopped.task.is_none());
    *signal.lifecycle_hook.lock().unwrap() = None;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_existing_durability_job_is_owned_until_join() {
    let fixture = Fixture::new();
    fixture.satellite.inner.lock().unwrap().federation_durable = false;
    let signal = &fixture.satellite.federation_executor_wake_signal;
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    *signal.lifecycle_hook.lock().unwrap() = Some(Arc::new(move |event| {
        if event == "durabilityRecoveryJob" {
            entered_tx.send(()).unwrap();
            phase_sync::receive(&released.lock().unwrap(), "release owned durability step");
        }
    }));
    let mut recovery = FederationDurabilityRecovery::default();
    recovery.next = Some(std::time::Instant::now());
    recovery.tick(&fixture.satellite);
    phase_sync::receive(&entered, "durability recovery owns its step");
    signal.stop();
    let mut join = Box::pin(recovery.shutdown());
    assert_join_pending(join.as_mut()).await;
    release.send(()).unwrap();
    join.await;
    *signal.lifecycle_hook.lock().unwrap() = None;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_m4_round_four_past_writer_receive_commits_once_and_shutdown_joins_before_persist() {
    let fixture = Arc::new(Fixture::new());
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].orchestrator_auto_dispatch_blocked = false;
    }
    let signal = fixture.satellite.federation_executor_wake_signal.clone();
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let (entered_tx, entered) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    *signal.lifecycle_hook.lock().unwrap() = Some(Arc::new(move |event| {
        observed.lock().unwrap().push(event);
        if event == "receiveAuthorized" {
            entered_tx.send(()).unwrap();
            phase_sync::receive(&released.lock().unwrap(), "release past-writer receive");
        }
    }));
    let runtime = no_external_runtime(&fixture.satellite);
    let receiver = fixture.clone();
    let response = tokio::spawn(async move { receiver.receive_raw(&receiver.request).await });
    phase_sync::receive(&entered, "authenticated receive holds writer and authority");
    let shutdown = tokio::spawn(runtime.shutdown());
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        while !signal.is_stopping() { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(*signal.receives.lock().unwrap(), 1);
    assert!(!shutdown.is_finished());
    release.send(()).unwrap();
    shutdown.await.unwrap();
    let response = response.await.unwrap();
    assert_eq!(response.0, StatusCode::ACCEPTED);
    assert_eq!(fixture.inbox().unwrap().stage, "received");
    assert_eq!(fixture.satellite.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_executor_operations", [], |row| row.get::<_, u64>(0)).unwrap(), 1);
    assert!(signal.activation_work.lock().unwrap().is_empty());
    assert!(fixture.queue(&fixture.satellite).is_empty());
    assert_eq!(*signal.receives.lock().unwrap(), 0);
    assert_eq!(signal.live.lock().unwrap().len(), 1, "captured intent is dormant and boot-local");
    fixture.satellite.shutdown_persist_blocking();
    let recorded = events.lock().unwrap().clone();
    let persist = recorded.iter().position(|event| *event == "persistShutdown").unwrap();
    for event in ["stopping", "drainJoined", "activationJoined", "receiverRecoveryJoined",
        "senderRecoveryJoined", "durabilityRecoveryJoined", "receivesJoined", "supervisorJoined"] {
        assert!(recorded.iter().position(|seen| *seen == event).unwrap() < persist, "{event}: {recorded:?}");
    }
    *signal.lifecycle_hook.lock().unwrap() = None;
}
