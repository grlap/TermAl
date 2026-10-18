// Owns connection-write failure, pending ownership and recovery regressions.
// Does not provision credentials or SSH. Split from federation_runtime.rs.
use super::*;

include!("federation_connect_writer.rs");

fn channel(app: &AppState, id: &str) -> FederationChannel {
    FederationChannel::for_app(
        app.federation_peer_snapshot(id).unwrap(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        app.clone(),
    )
}

fn scripted_channel(app: &AppState, id: &str) -> FederationChannel {
    let mut channel = channel(app, id);
    channel.durability_clock = EngramBudgetClock::scripted();
    channel
}

fn record(channel: &FederationChannel) -> tokio::task::JoinHandle<Result<(), ApiError>> {
    let lease = channel.activate_pending(Uuid::new_v4().to_string());
    let channel = channel.clone();
    tokio::spawn(async move { channel.record_connected(&lease).await })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_connect_write_failure_never_installs_an_unowned_link() {
    let fixture = Fixture::new().await;
    let channel = channel(&fixture.app, &fixture.id);
    let connection = rusqlite::Connection::open(fixture.app.persistence_path.as_path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_federation_write BEFORE INSERT ON app_state BEGIN SELECT RAISE(ABORT,'connection write failure'); END;").unwrap();
    assert!(record(&channel).await.unwrap().is_err());
    channel.invalidate();
    let orphan = fixture
        .app
        .inner
        .lock()
        .unwrap()
        .federation
        .links
        .contains_key(&fixture.id);
    connection
        .execute_batch("DROP TRIGGER reject_federation_write;")
        .unwrap();
    drop(connection);
    fixture.satellite_stop.send_replace(true);
    fixture.satellite_task.await.unwrap().unwrap();
    assert!(
        !orphan,
        "failed connection persistence must not publish a runtime link"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_connect_waits_for_durable_commit_before_link_publication() {
    let mut fixture = Fixture::new().await;
    let mut writer = ControlledWriter::attach(&mut fixture.app);
    let channel = scripted_channel(&fixture.app, &fixture.id);
    let task = record(&channel);
    let fence = writer.take().await;
    let early = fixture
        .app
        .inner
        .lock()
        .unwrap()
        .federation
        .links
        .contains_key(&fixture.id);
    ControlledWriter::commit(&fixture.app, &fence);
    assert!(task.await.unwrap().is_ok());
    assert!(channel.current_lease().is_ok());
    channel.invalidate();
    fixture.satellite_stop.send_replace(true);
    fixture.satellite_task.await.unwrap().unwrap();
    assert!(
        !early,
        "a pending durability fence grants no live-link ownership"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_connect_timeout_retains_basis_and_recovers_after_exact_reconciliation() {
    let mut fixture = Fixture::new().await;
    let mut writer = ControlledWriter::attach(&mut fixture.app);
    let channel = channel(&fixture.app, &fixture.id);
    let task = record(&channel);
    let held = writer.take().await;
    assert!(tokio::time::timeout(phase_sync::DEADLOCK_GUARD, task)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    let orphan = fixture
        .app
        .inner
        .lock()
        .unwrap()
        .federation
        .links
        .contains_key(&fixture.id);
    // A late write may commit after the original waiter expired. Only a fresh
    // content fence can settle the hold; never flip the flag in the fixture.
    ControlledWriter::commit(&fixture.app, &held);
    let app = fixture.app.clone();
    let reconcile = tokio::task::spawn_blocking(move || {
        app.reconcile_federation_durability_with_clock(EngramBudgetClock::scripted())
    });
    let fence = writer.take().await;
    ControlledWriter::commit(&fixture.app, &fence);
    reconcile.await.unwrap().unwrap();
    let reconnect = fixture
        .app
        .federation_reconnect_snapshot(&channel.transport_basis());
    fixture.satellite_stop.send_replace(true);
    fixture.satellite_task.await.unwrap().unwrap();
    assert!(!orphan, "timeout must not leave an unowned live UUID");
    assert!(
        reconnect.is_ok(),
        "own failed commit must retain its exact post-mutation basis"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_connect_cancelled_caller_retires_its_late_commit_without_clearing_successor() {
    let mut fixture = Fixture::new().await;
    let mut writer = ControlledWriter::attach(&mut fixture.app);
    let channel = scripted_channel(&fixture.app, &fixture.id);
    let task = record(&channel);
    let fence = writer.take().await;
    let prior = FederationLinkLease {
        channel: channel.clone(),
        incarnation: channel.state.lock().unwrap().incarnation.clone().unwrap(),
    };
    channel.invalidate();
    task.abort();
    let _ = task.await;
    ControlledWriter::commit(&fixture.app, &fence);
    // The owned completion must survive HTTP/task cancellation and retire
    // its exact late UUID. Wait on the resulting state, not a fixed sleep.
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            if fixture
                .app
                .inner
                .lock()
                .unwrap()
                .federation
                .links
                .is_empty()
                && channel.transport_basis().transport_generation
                    > channel.snapshot.transport_generation
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let next = record(&channel);
    let fence = writer.take().await;
    ControlledWriter::commit(&fixture.app, &fence);
    next.await.unwrap().unwrap();
    let successor = channel.current_lease().unwrap();
    channel.invalidate_if_current(&prior);
    assert_eq!(
        channel.current_lease().unwrap().incarnation,
        successor.incarnation
    );
    channel.invalidate();
    fixture.satellite_stop.send_replace(true);
    fixture.satellite_task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_connect_runtime_reconciles_actual_write_failure_without_manual_flag_reset() {
    let mut fixture = Fixture::new().await;
    let basis = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    let prior_uuid = Uuid::new_v4().to_string();
    let connected = fixture
        .app
        .federation_peer_connected(&basis, prior_uuid.clone())
        .unwrap();
    fixture
        .app
        .verify_federation_peer(&connected, federation_registry::checks())
        .unwrap();
    let verified = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    fixture
        .app
        .transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    let active = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    let retained = fixture
        .app
        .reserve_federation_placement(
            &active,
            "retained-connect".to_owned(),
            "original-session".to_owned(),
            1,
            0,
        )
        .unwrap();
    assert!(fixture
        .app
        .federation_peer_disconnected(&active, &prior_uuid));
    let connection = rusqlite::Connection::open(fixture.app.persistence_path.as_path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_federation_write BEFORE INSERT ON app_state BEGIN SELECT RAISE(ABORT,'connection write failure'); END;").unwrap();
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    let failed = fixture
        .event(|event| matches!(event, Event::Attempt(StatusCode::INTERNAL_SERVER_ERROR)))
        .await;
    connection
        .execute_batch("DROP TRIGGER reject_federation_write;")
        .unwrap();
    drop(connection);
    let recovered = fixture.event(|event| matches!(event, Event::Response(path, status) if path.ends_with("health") && status.is_success())).await;
    let durable = fixture.app.inner.lock().unwrap().federation_durable;
    assert_eq!(
        fixture.app.inner.lock().unwrap().federation.placements["retained-connect"],
        retained
    );
    assert!(fixture
        .app
        .ensure_federation_placement_current(&retained)
        .is_err());
    fixture.finish(runtime).await;
    assert!(failed);
    assert!(
        durable && recovered,
        "production supervisor must reconcile and establish a fresh link"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_connect_failure_recovery_preserves_observed_credential_material() {
    let mut fixture = Fixture::new().await;
    let read = fixture.io.credentials.clone();
    let app = fixture.app.clone();
    let rotation_ready = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release_rotation = ReleaseRotation(rotation_ready.clone());
    let current_rotated_read = Arc::new(AtomicBool::new(false));
    let observed_read = current_rotated_read.clone();
    fixture.io.credentials = Arc::new(move |snapshot| {
        let credentials = read(snapshot)?;
        if credentials.material_fingerprint == [2; 32] {
            let mut ready = rotation_ready.0.lock().unwrap();
            while !*ready {
                ready = rotation_ready.1.wait(ready).unwrap();
            }
        }
        // A generic stale-basis 409 is not a material refusal. Require the
        // rotated secret to be read against a durably CURRENT exact record.
        // No test mutation runs after this point, so the subsequent 409 can
        // only be the worker's retained material check.
        if credentials.material_fingerprint == [2; 32]
            && app.ensure_federation_peer_current(snapshot).is_ok()
        {
            observed_read.store(true, Ordering::SeqCst);
        }
        Ok(credentials)
    });
    let observer = fixture.io.attempt_finished.clone();
    let read_was_current = current_rotated_read.clone();
    let (refused_tx, mut refused_rx) = tokio::sync::mpsc::unbounded_channel();
    fixture.io.attempt_finished = Arc::new(move |result| {
        // Sample at the attempt's completion, not when a queued event is read:
        // a later current credential read must not relabel an earlier 409.
        if result
            .as_ref()
            .is_err_and(|error| error.status == StatusCode::CONFLICT)
            && read_was_current.load(Ordering::SeqCst)
        {
            let _ = refused_tx.send(());
        }
        observer(result);
    });
    let connection = rusqlite::Connection::open(fixture.app.persistence_path.as_path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_federation_write BEFORE INSERT ON app_state BEGIN SELECT RAISE(ABORT,'connection write failure'); END;").unwrap();
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    assert!(
        fixture
            .event(|event| matches!(event, Event::Attempt(StatusCode::INTERNAL_SERVER_ERROR)))
            .await
    );
    fixture.fingerprint.store(2, Ordering::SeqCst);
    connection
        .execute_batch("DROP TRIGGER reject_federation_write;")
        .unwrap();
    drop(connection);
    // Isolate material retention from the other orphan/recovery regressions:
    // settle the actual failed image and retire any failed UUID using M1's
    // exact post-basis CAS. The separate runtime test forbids manual recovery.
    let app = fixture.app.clone();
    tokio::task::spawn_blocking(move || {
        app.reconcile_federation_durability_with_clock(EngramBudgetClock::scripted())
    })
    .await
    .unwrap()
    .unwrap();
    let current = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    let failed_uuid = fixture
        .app
        .inner
        .lock()
        .unwrap()
        .federation
        .links
        .get(&fixture.id)
        .cloned();
    if let Some(uuid) = failed_uuid {
        assert!(fixture.app.federation_peer_disconnected(&current, &uuid));
    }
    // No replacement may complete its rotated read before exact orphan
    // retirement; otherwise the fixture could accidentally target a successor.
    drop(release_rotation);
    let refused = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, refused_rx.recv())
        .await
        .ok()
        .flatten()
        .is_some();
    let no_replacement_channel = runtime.peer(&fixture.id).is_none();
    fixture.finish(runtime).await;
    assert!(
        refused && current_rotated_read.load(Ordering::SeqCst) && no_replacement_channel,
        "a failed own connect must not reset the observed material fence"
    );
}
