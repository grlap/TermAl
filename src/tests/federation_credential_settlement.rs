// Deterministic ownership and late-completion checks; no OS store or SSH.
use super::*;

struct ReadGate(Arc<(Mutex<bool>, std::sync::Condvar)>);

impl ReadGate {
    fn new() -> Self {
        Self(Arc::new((Mutex::new(false), std::sync::Condvar::new())))
    }

    fn release(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

impl Drop for ReadGate {
    fn drop(&mut self) {
        self.release();
    }
}

fn wait_at(gate: &Arc<(Mutex<bool>, std::sync::Condvar)>) {
    let mut released = gate.0.lock().unwrap();
    while !*released {
        released = gate.1.wait(released).unwrap();
    }
}

async fn next<T>(events: &mut tokio::sync::mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, events.recv())
        .await
        .unwrap()
        .expect("expected phase event")
}

fn pending_events(io: &mut FederationRuntimeIo) -> tokio::sync::mpsc::UnboundedReceiver<()> {
    let (send, receive) = tokio::sync::mpsc::unbounded_channel();
    io.credential_observed = Arc::new(move |state| {
        if state.starts_with("pending;") {
            let _ = send.send(());
        }
    });
    io.credential_reporting_deadline = Duration::ZERO;
    receive
}

fn basis() -> (FederationPeerSnapshot, Arc<FederationTransportBasis>) {
    let app = federation_registry::central();
    let record = federation_registry::peer(47003);
    let id = record.identity.host_id.clone();
    app.enroll_federation_peer(record).unwrap();
    let snapshot = app.federation_peer_snapshot(&id).unwrap();
    let basis = Arc::new(FederationTransportBasis::new(snapshot.clone()));
    (snapshot, basis)
}

fn material() -> FederationTransportCredentials {
    FederationTransportCredentials {
        capability: FederationPairCapability::new("a".repeat(64)).unwrap(),
        known_hosts: None,
        material_fingerprint: [1; 32],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_same_basis_read_recovers_once_without_listener_or_duplicate_read() {
    let mut fixture = Fixture::new().await;
    let gate = ReadGate::new();
    let held = gate.0.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let reads = calls.clone();
    let read = fixture.io.credentials.clone();
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    fixture.io.credentials = Arc::new(move |snapshot| {
        reads.fetch_add(1, Ordering::SeqCst);
        let _ = entered.send(());
        wait_at(&held);
        read(snapshot)
    });
    let mut pending = pending_events(&mut fixture.io);
    let (cycle, mut cycles) = tokio::sync::mpsc::unbounded_channel();
    fixture.io.reconciled = Arc::new(move |_| {
        let _ = cycle.send(());
    });
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    next(&mut entries).await;
    next(&mut pending).await;
    // Successive reconciliations cannot replace this pending owner.
    next(&mut cycles).await;
    next(&mut cycles).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(runtime.peer(&fixture.id).is_none());
    let port = fixture
        .app
        .federation_peer_snapshot(&fixture.id)
        .unwrap()
        .endpoint
        .central_federation_port;
    let vacant = std::net::TcpListener::bind(("127.0.0.1", port))
        .expect("pending read must not bind a listener");
    drop(vacant);
    assert!(runtime.quarantined.lock().unwrap().is_empty());
    gate.release();
    assert!(
        fixture
            .event(|event| matches!(event, Event::Response(path, status)
        if path.ends_with("health") && status.is_success()))
            .await
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    fixture.finish(runtime).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_read_blocks_successor_across_reenrollment_and_disable_enable() {
    for disable_enable in [false, true] {
        let mut fixture = Fixture::new().await;
        let gate = ReadGate::new();
        let held = gate.0.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let reads = calls.clone();
        let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
        fixture.io.credentials = Arc::new(move |snapshot| {
            let index = reads.fetch_add(1, Ordering::SeqCst);
            let _ = entered.send(snapshot.clone());
            if index == 0 {
                wait_at(&held);
                Ok(material())
            } else {
                Err(federation_credential_error())
            }
        });
        let starts = Arc::new(AtomicUsize::new(0));
        let ssh_starts = starts.clone();
        fixture.io.ssh = Arc::new(move |_, _, _| {
            ssh_starts.fetch_add(1, Ordering::SeqCst);
            Err(federation_transport_unavailable())
        });
        let mut pending = pending_events(&mut fixture.io);
        let (cycle, mut cycles) = tokio::sync::mpsc::unbounded_channel();
        fixture.io.reconciled = Arc::new(move |_| {
            let _ = cycle.send(());
        });
        let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
        let original = next(&mut entries).await;
        next(&mut pending).await;
        while cycles.try_recv().is_ok() {}
        if disable_enable {
            let settings = fixture
                .app
                .inner
                .lock()
                .unwrap()
                .preferences
                .federation
                .clone();
            let mut disabled = settings.clone();
            disabled.enabled = false;
            fixture.app.configure_federation(disabled).unwrap();
            // The old read remains owned even while the desired set is empty.
            next(&mut cycles).await;
            next(&mut cycles).await;
            fixture.app.configure_federation(settings).unwrap();
        } else {
            let mut record =
                fixture.app.inner.lock().unwrap().federation.peers[&fixture.id].clone();
            record.endpoint.ssh_user = "new-generation".to_owned();
            fixture.app.enroll_federation_peer(record).unwrap();
        }
        next(&mut cycles).await;
        next(&mut cycles).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(runtime.peer(&fixture.id).is_none());
        gate.release();
        let successor = next(&mut entries).await;
        assert!(!successor.same_transport_as(&original));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        assert!(runtime.peer(&fixture.id).is_none());
        assert!(runtime.quarantined.lock().unwrap().is_empty());
        fixture.finish(runtime).await;
    }
}

#[tokio::test]
async fn queued_result_owns_guard_until_adoption_or_destruction() {
    for adopt in [false, true] {
        let (snapshot, basis) = basis();
        let (done, receive) = tokio::sync::oneshot::channel();
        let read = FederationCredentialRead::start_with(
            snapshot,
            &basis,
            Arc::new(|_| Ok(material())),
            move |job| {
                std::thread::spawn(move || {
                    job();
                    let _ = done.send(());
                });
                Ok(())
            },
        )
        .unwrap();
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, receive)
            .await
            .unwrap()
            .unwrap();
        assert!(
            basis.is_settling(),
            "successful send must not settle queued ownership"
        );
        if adopt {
            let (_stop, mut stopped) = tokio::sync::watch::channel(false);
            let result = read
                .wait(&mut stopped, Duration::ZERO, |_| {})
                .await
                .unwrap()
                .unwrap();
            assert!(
                basis.is_settling(),
                "adoption validation still owns the result"
            );
            assert!(result.result.is_ok());
            drop(result);
        } else {
            drop(read);
        }
        assert!(!basis.is_settling());
    }
}

#[tokio::test]
async fn receiver_loss_retains_guard_until_original_reader_finishes() {
    let (snapshot, basis) = basis();
    let gate = ReadGate::new();
    let held = gate.0.clone();
    let (began, mut entered) = tokio::sync::mpsc::unbounded_channel();
    let (done, receive) = tokio::sync::oneshot::channel();
    let read = FederationCredentialRead::start_with(
        snapshot,
        &basis,
        Arc::new(move |_| {
            let _ = began.send(());
            wait_at(&held);
            Ok(material())
        }),
        move |job| {
            std::thread::spawn(move || {
                job();
                let _ = done.send(());
            });
            Ok(())
        },
    )
    .unwrap();
    next(&mut entered).await;
    drop(read);
    assert!(basis.is_settling());
    gate.release();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, receive)
        .await
        .unwrap()
        .unwrap();
    assert!(!basis.is_settling());
}

#[tokio::test]
async fn stopped_waiter_discards_queued_success_without_adoption() {
    let (snapshot, basis) = basis();
    let (done, receive) = tokio::sync::oneshot::channel();
    let read = FederationCredentialRead::start_with(
        snapshot,
        &basis,
        Arc::new(|_| Ok(material())),
        move |job| {
            std::thread::spawn(move || {
                job();
                let _ = done.send(());
            });
            Ok(())
        },
    )
    .unwrap();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, receive)
        .await
        .unwrap()
        .unwrap();
    let (_stop, mut stopped) = tokio::sync::watch::channel(true);
    let result = read
        .wait(&mut stopped, Duration::ZERO, |_| {})
        .await
        .unwrap();
    assert!(result.is_none());
    assert!(!basis.is_settling());
}

#[tokio::test]
async fn spawn_error_and_reader_panic_release_only_the_credential_guard() {
    let (snapshot, basis) = basis();
    let calls = Arc::new(AtomicUsize::new(0));
    let reads = calls.clone();
    let result = FederationCredentialRead::start_with(
        snapshot.clone(),
        &basis,
        Arc::new(move |_| {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(material())
        }),
        |job| {
            assert!(basis.is_settling());
            drop(job);
            Err(std::io::Error::other("injected spawn failure"))
        },
    );
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!basis.is_settling());
    let (done, receive) = tokio::sync::oneshot::channel();
    let read = FederationCredentialRead::start_with(
        snapshot,
        &basis,
        Arc::new(|_| panic!("injected reader panic")),
        move |job| {
            std::thread::spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                let _ = done.send(());
            });
            Ok(())
        },
    )
    .unwrap();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, receive)
        .await
        .unwrap()
        .unwrap();
    let (_stop, mut stopped) = tokio::sync::watch::channel(false);
    let mut states = Vec::new();
    assert!(
        read.wait(&mut stopped, Duration::ZERO, |state| states.push(state))
            .await
            .is_err()
    );
    assert_eq!(states, ["worker result lost; no credentials adopted"]);
    assert!(!basis.is_settling());
}

#[tokio::test]
async fn completed_error_is_redacted_and_retains_queued_ownership() {
    let (snapshot, basis) = basis();
    let read = FederationCredentialRead::start(
        snapshot,
        &basis,
        Arc::new(|_| Err(ApiError::internal("injected secret must not be diagnosed"))),
    )
    .unwrap();
    let (_stop, mut stopped) = tokio::sync::watch::channel(false);
    let mut states = Vec::new();
    let result = read
        .wait(&mut stopped, Duration::from_secs(5), |state| {
            states.push(state)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(states, ["completed failure"]);
    assert!(basis.is_settling());
    assert!(result.result.is_err());
    drop(result);
    assert!(!basis.is_settling());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_success_rechecks_stop_and_basis_after_completion() {
    for stop_on_completion in [false, true] {
        let mut fixture = Fixture::new().await;
        let gate = ReadGate::new();
        let held = gate.0.clone();
        fixture.io.credentials = Arc::new(move |_| {
            wait_at(&held);
            Ok(material())
        });
        fixture.io.credential_reporting_deadline = Duration::ZERO;
        let (event, mut events) = tokio::sync::mpsc::unbounded_channel();
        let app = fixture.app.clone();
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        fixture.io.credential_observed = Arc::new(move |state| {
            if state.starts_with("completed after pending;") {
                if stop_on_completion {
                    stop.send_replace(true);
                } else {
                    let mut settings = app.inner.lock().unwrap().preferences.federation.clone();
                    settings.enabled = false;
                    app.configure_federation(settings).unwrap();
                }
            }
            let _ = event.send(state);
        });
        let snapshot = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
        let basis = Arc::new(FederationTransportBasis::new(snapshot.clone()));
        let peers = FederationPeerHandles::default();
        let (app, io, captured, registry) = (
            fixture.app.clone(),
            fixture.io.clone(),
            basis.clone(),
            peers.clone(),
        );
        let serve = tokio::spawn(async move {
            federation_serve_peer(
                &app,
                &snapshot,
                captured,
                &registry,
                &mut stopped,
                &mut FederationBackoff::default(),
                &mut FederationAttemptState::default(),
                &io,
            )
            .await
        });
        assert!(next(&mut events).await.starts_with("pending;"));
        gate.release();
        let result = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, serve)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status, StatusCode::CONFLICT);
        assert!(peers.lock().unwrap().is_empty());
        assert!(!basis.is_settling());
        fixture.satellite_stop.send_replace(true);
        fixture.satellite_task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn aborted_waiter_retains_unsettled_operation_until_thread_destroys_result() {
    let (snapshot, basis) = basis();
    let gate = ReadGate::new();
    let held = gate.0.clone();
    let (done, receive) = tokio::sync::oneshot::channel();
    let read = FederationCredentialRead::start_with(
        snapshot,
        &basis,
        Arc::new(move |_| {
            wait_at(&held);
            Ok(material())
        }),
        move |job| {
            std::thread::spawn(move || {
                job();
                let _ = done.send(());
            });
            Ok(())
        },
    )
    .unwrap();
    let (_stop, mut stopped) = tokio::sync::watch::channel(false);
    let (pending, mut events) = tokio::sync::mpsc::unbounded_channel();
    let waiter = tokio::spawn(async move {
        read.wait(&mut stopped, Duration::ZERO, |state| {
            let _ = pending.send(state);
        })
        .await
    });
    assert!(next(&mut events).await.starts_with("pending;"));
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    assert!(basis.is_settling());
    gate.release();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, receive)
        .await
        .unwrap()
        .unwrap();
    assert!(!basis.is_settling());
}

#[test]
fn refreshed_bookkeeping_may_start_a_read_but_captured_generation_must_stay_current() {
    let app = federation_registry::central();
    let record = federation_registry::peer(47003);
    let id = record.identity.host_id.clone();
    app.enroll_federation_peer(record).unwrap();
    let original = app.federation_peer_snapshot(&id).unwrap();
    let basis = Arc::new(FederationTransportBasis::new(original.clone()));
    // Isolate a bookkeeping change without adopting another transport basis.
    app.inner
        .lock()
        .unwrap()
        .federation
        .peers
        .get_mut(&id)
        .unwrap()
        .record_generation += 1;
    let current = app.federation_reconnect_snapshot(&original).unwrap();
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    assert!(federation_validate_credential_basis(&app, &current, &basis, &stopped).is_ok());
    assert!(federation_validate_credential_basis(&app, &original, &basis, &stopped).is_err());
    app.inner
        .lock()
        .unwrap()
        .federation
        .peers
        .get_mut(&id)
        .unwrap()
        .record_generation += 1;
    assert!(federation_validate_credential_basis(&app, &current, &basis, &stopped).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_material_change_preserves_the_installed_material_fence() {
    let mut fixture = Fixture::new().await;
    let gate = ReadGate::new();
    let held = gate.0.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let reads = calls.clone();
    let read = fixture.io.credentials.clone();
    let (entered, mut entries) = tokio::sync::mpsc::unbounded_channel();
    fixture.io.credentials = Arc::new(move |snapshot| {
        if reads.fetch_add(1, Ordering::SeqCst) == 1 {
            let _ = entered.send(());
            wait_at(&held);
        }
        read(snapshot)
    });
    let starts = Arc::new(AtomicUsize::new(0));
    let ssh_starts = starts.clone();
    let start = fixture.io.ssh.clone();
    fixture.io.ssh = Arc::new(move |snapshot, port, pin| {
        ssh_starts.fetch_add(1, Ordering::SeqCst);
        start(snapshot, port, pin)
    });
    let mut pending = pending_events(&mut fixture.io);
    let runtime = start_federation_runtime_with_io(fixture.app.clone(), fixture.io.clone());
    assert!(
        fixture
            .event(|event| matches!(event, Event::Response(path, status)
        if path.ends_with("health") && status.is_success()))
            .await
    );
    // Discard any first-read deadline observation, then hold the reconnect read.
    while pending.try_recv().is_ok() {}
    fixture.fingerprint.store(2, Ordering::SeqCst);
    fixture.fail_health.store(true, Ordering::SeqCst);
    next(&mut entries).await;
    next(&mut pending).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    gate.release();
    assert!(
        fixture
            .event(|event| matches!(event, Event::Attempt(StatusCode::CONFLICT)))
            .await
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    fixture.finish(runtime).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_change_after_bind_cannot_publish_a_handle_or_start_ssh() {
    let mut fixture = Fixture::new().await;
    let app = fixture.app.clone();
    fixture.io.prepare_forward = Some(Arc::new(move |_| {
        let mut settings = app.inner.lock().unwrap().preferences.federation.clone();
        settings.enabled = false;
        app.configure_federation(settings).unwrap();
        Ok(47004)
    }));
    let snapshot = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    let basis = Arc::new(FederationTransportBasis::new(snapshot.clone()));
    let peers = FederationPeerHandles::default();
    let (_stop, mut stopped) = tokio::sync::watch::channel(false);
    let result = federation_serve_peer(
        &fixture.app,
        &snapshot,
        basis.clone(),
        &peers,
        &mut stopped,
        &mut FederationBackoff::default(),
        &mut FederationAttemptState::default(),
        &fixture.io,
    )
    .await;
    assert!(result.is_err());
    assert!(peers.lock().unwrap().is_empty());
    assert!(!basis.is_settling());
    fixture.satellite_stop.send_replace(true);
    fixture.satellite_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn guard_acquired_after_removal_cut_survives_worker_join_and_shutdown() {
    let (snapshot, basis) = basis();
    let gate = ReadGate::new();
    let held = gate.0.clone();
    let (done, receive) = tokio::sync::oneshot::channel();
    let captured = basis.clone();
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        federation_wait_stop(&mut stopped).await;
        // Model the read entering just after the supervisor's removal cut.
        let read = FederationCredentialRead::start_with(
            snapshot,
            &captured,
            Arc::new(move |_| {
                wait_at(&held);
                Ok(material())
            }),
            move |job| {
                std::thread::spawn(move || {
                    job();
                    let _ = done.send(());
                });
                Ok(())
            },
        )
        .unwrap();
        drop(read);
    });
    let mut worker = FederationWorker {
        basis: basis.clone(),
        stop,
        task,
        joined: false,
    };
    assert!(
        !worker.basis.is_settling(),
        "supervisor removal cut precedes read acquisition"
    );
    worker.stop.send_replace(true);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, worker.join())
        .await
        .unwrap();
    assert!(worker.joined);
    assert!(
        worker.basis.is_settling(),
        "joined task cannot authorize its replacement yet"
    );
    // Shutdown of the already joined tombstone is bounded, not a second poll
    // of the completed JoinHandle and not a join of the blocked OS thread.
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, worker.join())
        .await
        .unwrap();
    assert!(worker.basis.is_settling());
    gate.release();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, receive)
        .await
        .unwrap()
        .unwrap();
    assert!(!worker.basis.is_settling());
}
