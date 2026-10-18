// Owns startup/reconfiguration of per-peer federation workers and their handles.
// Does not mutate enrollment, start satellite hosts or expose local APIs.
// New composition beside main.rs/run_server, not an executor-proxy refactor.

struct FederationPeerHandle {
    channel: FederationChannel,
    // Consumed by later federation RPC slices; the production monitor uses
    // this same client and the lifecycle tests exercise this exported handle.
    #[cfg_attr(not(test), allow(dead_code))]
    rpc: FederationRpcClient,
}

type FederationPeerHandles = Arc<Mutex<HashMap<String, Arc<FederationPeerHandle>>>>;
type FederationQuarantinedPeers = Arc<Mutex<HashSet<String>>>;

struct FederationWorker {
    basis: Arc<FederationTransportBasis>,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

struct FederationRuntime {
    #[cfg_attr(not(test), allow(dead_code))]
    peers: FederationPeerHandles,
    #[cfg_attr(not(test), allow(dead_code))]
    quarantined: FederationQuarantinedPeers,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl FederationRuntime {
    async fn shutdown(self) {
        self.stop.send_replace(true);
        if let Err(error) = self.task.await {
            eprintln!("federation supervisor failed during shutdown: {error}");
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn peer(&self, peer_host_id: &str) -> Option<Arc<FederationPeerHandle>> {
        self.peers
            .lock()
            .expect("federation registry mutex poisoned")
            .get(peer_host_id)
            .cloned()
    }
}

fn start_federation_runtime(app: AppState) -> FederationRuntime {
    start_federation_runtime_with_io(app, FederationRuntimeIo::default())
}

fn start_federation_runtime_with_io(app: AppState, io: FederationRuntimeIo) -> FederationRuntime {
    let peers = FederationPeerHandles::default();
    let quarantined = FederationQuarantinedPeers::default();
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(federation_supervise(
        app,
        peers.clone(),
        quarantined.clone(),
        stop_rx,
        io,
    ));
    FederationRuntime {
        peers,
        quarantined,
        stop,
        task,
    }
}

async fn federation_supervise(
    app: AppState,
    peers: FederationPeerHandles,
    quarantined: FederationQuarantinedPeers,
    mut stop: tokio::sync::watch::Receiver<bool>,
    io: FederationRuntimeIo,
) {
    let mut host_stop = app.subscribe_shutdown_signal();
    let mut workers = HashMap::<String, FederationWorker>::new();
    let mut durability = FederationDurabilityRecovery::default();
    loop {
        if *stop.borrow() || *host_stop.borrow() {
            break;
        }
        durability.tick(&app);
        // The snapshot API releases StateInner before returning. Disabled and
        // standalone settings return no peers: no bind, secret lookup or SSH.
        let snapshots = match app.federation_peer_snapshots() {
            Ok(snapshots) => snapshots,
            Err(error) => {
                eprintln!("federation configuration unavailable: {}", error.message);
                // A failed authoritative read is not an empty registry. In
                // particular, a pending durability fence must not tear down
                // a healthy channel and invalidate verification in a loop.
                tokio::select! {
                    _ = federation_wait_stop(&mut stop) => break,
                    _ = federation_wait_stop(&mut host_stop) => break,
                    _ = tokio::time::sleep(io.reconcile_interval) => {}
                }
                continue;
            }
        };
        let desired: HashMap<_, _> = snapshots
            .into_iter()
            .filter(|snapshot| {
                !quarantined
                    .lock()
                    .expect("federation quarantine mutex poisoned")
                    .contains(&snapshot.peer.host_id)
            })
            .map(|snapshot| (snapshot.peer.host_id.clone(), snapshot))
            .collect();
        #[cfg(test)]
        (io.reconciled)(desired.len());
        let removed: Vec<_> = workers
            .iter()
            .filter(|(id, worker)| {
                // Detached connection completion owns this marker even after
                // the caller or peer handle is gone. Check before task state.
                if worker.basis.is_settling() {
                    return false;
                }
                if worker.task.is_finished() {
                    return true;
                }
                let channel = peers
                    .lock()
                    .expect("federation registry mutex poisoned")
                    .get(*id)
                    .map(|peer| peer.channel.clone());
                let basis = worker
                    .basis
                    .read()
                    .expect("federation basis lock poisoned")
                    .clone();
                // An own durable connection commit may be visible at M1 just
                // before its exact returned basis is installed here. Admission
                // remains fenced; let that serialized transition settle.
                let connecting = channel
                    .as_ref()
                    .is_some_and(|channel| channel.opening.try_lock().is_err());
                if connecting
                    || desired
                        .get(*id)
                        .is_some_and(|next| basis.same_transport_as(next))
                {
                    return false;
                }
                // The enumerated desired cut can also PRECEDE our own commit.
                // Keep only a channel that M1 validates NOW for its exact UUID;
                // never refresh from the stale enumeration or adopt a config.
                !channel
                    .as_ref()
                    .is_some_and(|channel| channel.current_authority().is_ok())
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &removed {
            if let Some(worker) = workers.get(id) {
                worker.stop.send_replace(true);
            }
        }
        for id in removed {
            if let Some(worker) = workers.remove(&id) {
                if let Err(error) = worker.task.await {
                    eprintln!("federation worker failed: {error}");
                }
                peers
                    .lock()
                    .expect("federation registry mutex poisoned")
                    .remove(&id);
            }
        }
        for (id, snapshot) in desired {
            if workers.contains_key(&id) {
                continue;
            }
            let (peer_stop, peer_stop_rx) = tokio::sync::watch::channel(false);
            let basis = Arc::new(FederationTransportBasis::new(snapshot));
            let task = tokio::spawn(federation_run_peer(
                app.clone(),
                basis.clone(),
                peers.clone(),
                quarantined.clone(),
                peer_stop_rx,
                io.clone(),
            ));
            workers.insert(
                id,
                FederationWorker {
                    basis,
                    stop: peer_stop,
                    task,
                },
            );
        }
        tokio::select! {
            _ = federation_wait_stop(&mut stop) => break,
            _ = federation_wait_stop(&mut host_stop) => break,
            _ = tokio::time::sleep(io.reconcile_interval) => {}
        }
    }
    // Reap every old worker before the runtime reports shutdown complete.
    for worker in workers.values() {
        worker.stop.send_replace(true);
    }
    for (_, worker) in workers {
        if let Err(error) = worker.task.await {
            eprintln!("federation worker failed during shutdown: {error}");
        }
    }
    peers
        .lock()
        .expect("federation registry mutex poisoned")
        .clear();
}

async fn federation_run_peer(
    app: AppState,
    basis: Arc<FederationTransportBasis>,
    peers: FederationPeerHandles,
    quarantined: FederationQuarantinedPeers,
    mut stop: tokio::sync::watch::Receiver<bool>,
    io: FederationRuntimeIo,
) {
    let mut retry = FederationBackoff::default();
    let mut attempt = FederationAttemptState::default();
    loop {
        if *stop.borrow() {
            return;
        }
        if basis.is_settling() {
            tokio::select! {
                _ = federation_wait_stop(&mut stop) => return,
                _ = tokio::time::sleep(io.reconcile_interval) => {}
            }
            continue;
        }
        let expected = basis
            .read()
            .expect("federation basis lock poisoned")
            .clone();
        let result = match app.federation_reconnect_snapshot(&expected) {
            Ok(snapshot) => {
                federation_serve_peer(
                    &app,
                    &snapshot,
                    basis.clone(),
                    &peers,
                    &mut stop,
                    &mut retry,
                    &mut attempt,
                    &io,
                )
                .await
            }
            Err(error) => Err(error),
        };
        if let Err(ref error) = result {
            // Do not log request bodies, raw SSH stderr, secret references or
            // externally supplied error bodies. Typed status is sufficient.
            eprintln!(
                "federation peer {} unavailable ({})",
                expected.peer.host_id, error.status
            );
            let cleanup_unknown =
                matches!(error.kind, Some(ApiErrorKind::FederationSshCleanupUnknown));
            if cleanup_unknown || attempt.unresolved.load(Ordering::SeqCst) {
                quarantined
                    .lock()
                    .expect("federation quarantine mutex poisoned")
                    .insert(expected.peer.host_id.clone());
                eprintln!(
                    "federation peer {} quarantined for this runtime: owned OS operation unresolved",
                    expected.peer.host_id
                );
            }
            #[cfg(test)]
            (io.attempt_finished)(&result);
            if cleanup_unknown
                || attempt.unresolved.load(Ordering::SeqCst)
                || attempt.material_changed
            {
                if attempt.material_changed {
                    eprintln!(
                        "federation peer {} paused: enrolled credential material changed",
                        expected.peer.host_id
                    );
                }
                // Only locally proven hard fences park. Remote 409/403 and
                // temporary durability holds remain bounded retry outcomes.
                federation_wait_stop(&mut stop).await;
                return;
            }
        }
        if *stop.borrow() {
            return;
        }
        let delay = retry.next_delay();
        retry.record_failure();
        tokio::select! {
            _ = federation_wait_stop(&mut stop) => return,
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

async fn federation_wait_stop(stop: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}
