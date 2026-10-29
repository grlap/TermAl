// Owns the candidate loopback router and an actual SQLite fence writer.
// Teardown releases every worker before the repository-local root is removed.

struct HistoryService {
    state: Option<AppState>,
    runtime: Option<tokio::runtime::Runtime>,
    server: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    writer: Option<std::thread::JoinHandle<()>>,
    broadcaster: Option<std::thread::JoinHandle<()>>,
    broadcast_stop: Arc<AtomicBool>,
    broadcast_barrier: mpsc::Receiver<()>,
    events: broadcast::Receiver<SseStreamEvent>,
    scan: Option<std::thread::JoinHandle<HistoryReads>>,
    release_id: Option<String>,
    commits: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    // One-shot schedule hook: the writer reports it reached its next tick,
    // then waits for release before draining or collecting anything.
    writer_gate: Arc<Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>>,
    root: PathBuf,
    cleaned: mpsc::Receiver<std::result::Result<(), String>>,
    url: String,
}

impl HistoryService {
    fn new(mut state: AppState) -> Self {
        let root = state.test_temp_root.as_ref().unwrap().path().to_owned();
        let cleaned = state.test_temp_root.as_ref().unwrap().observe_cleanup();
        // Reload the real baseline before serving; no registration state is
        // fabricated. Enabling the durable card epoch uses the ordinary path.
        state.inner = Arc::new(StateMutex::new(
            load_state(state.persistence_path.as_path()).unwrap().unwrap()));
        assert!(state.ensure_test_run_cards_epoch());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2).enable_all().build().unwrap();
        let (tx, rx) = mpsc::channel();
        state.persist_tx = tx;
        let mailbox = Arc::new(StateBroadcastMailbox::default());
        state.state_broadcast_mailbox = Some(mailbox.clone());
        let events = state.subscribe_stream_events();
        let broadcast_stop = Arc::new(AtomicBool::new(false));
        let (barrier_tx, broadcast_barrier) = mpsc::channel();
        let mut service = Self {
            state: Some(state), runtime: Some(runtime), server: None, stop: None,
            writer: None, scan: None, release_id: None,
            broadcaster: None, broadcast_stop, broadcast_barrier, events,
            commits: Default::default(), writer_gate: Default::default(),
            root, cleaned, url: String::new(),
        };
        let stop = service.broadcast_stop.clone();
        let senders = service.state.as_ref().unwrap().state_broadcast_senders.clone();
        service.broadcaster = Some(std::thread::spawn(move || loop {
            let work = mailbox.recv_next();
            if stop.load(Ordering::SeqCst) { break; }
            let barrier = matches!(&work, StateBroadcastWork::Delta(event)
                if event.revision() == u64::MAX);
            forward_state_broadcast_work(work, &senders);
            if barrier { let _ = barrier_tx.send(()); }
        }));
        let state = service.state.as_ref().unwrap();
        let inner = state.inner.clone();
        let path = state.persistence_path.clone();
        let commits = service.commits.clone();
        let gate = service.writer_gate.clone();
        service.writer = Some(std::thread::spawn(move || {
            TEST_HISTORY_COMMITTED.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |delta| {
                    let mut commits = commits.lock().unwrap();
                    for id in delta.metadata.test_run_registrations.keys() {
                        commits.entry(id.clone()).or_insert_with(std::time::Instant::now);
                    }
                }));
            });
            let mut cache = SqlitePersistConnectionCache::new();
            let mut fences = PersistFenceBatch::default();
            let retry = PersistWorkerRetryState::default();
            let mut watermark = 0;
            loop {
                let outcome = retry.wait_for_next_tick(&rx, &mut fences);
                if outcome == PersistWorkerWaitOutcome::Exit {
                    fences.fail(PersistFenceError::WorkerStopped);
                    break;
                }
                let held = gate.lock().unwrap().take();
                if let Some((reached, release)) = held {
                    let _ = reached.send(());
                    let _ = release.recv();
                }
                let shutdown = fences.drain(&rx, outcome == PersistWorkerWaitOutcome::Shutdown);
                fences.mark_in_tick();
                let delta = collect_persist_delta_from_shared_state(&inner, watermark);
                let persisted = persist_delta_with_fences(&mut cache, &path, &delta, &mut fences)
                    .expect("history writer must commit its actual SQLite delta");
                inner.lock().unwrap().trim_persisted_session_tails(delta.watermark, &persisted);
                watermark = delta.watermark;
                if shutdown { break; }
            }
            TEST_HISTORY_COMMITTED.with(|slot| slot.borrow_mut().take());
        }));
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let router = app_router(service.state.as_ref().unwrap().clone());
        let runtime = service.runtime.as_ref().unwrap();
        let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        service.url = format!("http://{}", listener.local_addr().unwrap());
        service.stop = Some(stop);
        service.server = Some(runtime.spawn(async move {
            axum::serve(listener, router).with_graceful_shutdown(async { let _ = stopped.await; }).await
        }));
        service
    }

    /// Holds the writer at its next tick: returns the receiver that reports
    /// the writer reached it and the sender that releases it. Dropping the
    /// sender also releases it.
    fn hold_next_writer_tick(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        assert!(self.writer_gate.lock().unwrap().replace((reached_tx, release_rx)).is_none());
        (reached_rx, release_tx)
    }

    fn serialized_failure_events(&mut self) -> String {
        // FIFO drain marker only, matching the established publication fixture.
        // It supplies no state, SQL completion, revision, or registration proof.
        self.state.as_ref().unwrap().state_broadcast_mailbox.as_ref().unwrap()
            .publish_delta(DeltaEvent::CodexUpdated { revision: u64::MAX, codex: CodexState::default() });
        self.broadcast_barrier.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        let mut observed = String::new();
        loop {
            match self.events.try_recv() {
                Ok(SseStreamEvent::Delta(payload)) => observed.push_str(&payload),
                Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => (),
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(error) => panic!("history event stream closed: {error:?}"),
            }
        }
        observed
    }

    fn stop_all(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        // Closing an unconsumed handshake releases the scanner on every panic
        // path. It keeps its normal serialization guard until it returns.
        if let Some(id) = self.release_id.take() {
            test_history_received_hooks().lock().unwrap().remove(&id);
        }
        if let Some(scan) = self.scan.take() {
            if scan.join().is_err() { errors.push("history scan panicked".to_owned()); }
        }
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        if let Some(server) = self.server.take() {
            let result = self.runtime.as_ref().unwrap().block_on(async {
                tokio::time::timeout(phase_sync::DEADLOCK_GUARD, server).await
            });
            if !matches!(result, Ok(Ok(Ok(())))) {
                errors.push(format!("history server shutdown: {result:?}"));
            }
        }
        if let Some(state) = self.state.as_ref() { let _ = state.persist_tx.send(PersistRequest::Shutdown); }
        if let Some(writer) = self.writer.take() {
            if writer.join().is_err() { errors.push("history writer panicked".to_owned()); }
        }
        self.broadcast_stop.store(true, Ordering::SeqCst);
        if let Some(state) = self.state.as_ref() {
            state.state_broadcast_mailbox.as_ref().unwrap().publish_delta(
                DeltaEvent::CodexUpdated { revision: u64::MAX, codex: CodexState::default() });
        }
        if let Some(worker) = self.broadcaster.take() {
            if worker.join().is_err() { errors.push("history broadcaster panicked".to_owned()); }
        }
        if let Some(runtime) = self.runtime.take() { runtime.shutdown_timeout(phase_sync::DEADLOCK_GUARD); }
        self.state.take();
        match self.cleaned.recv_timeout(phase_sync::DEADLOCK_GUARD) {
            Ok(Ok(())) => (),
            other => errors.push(format!("history cleanup receipt: {other:?}")),
        }
        if self.root.exists() || TestTempRoot::owner_marker(&self.root).exists() {
            errors.push("history root or owner marker survived teardown".to_owned());
        }
        errors
    }
}

impl Drop for HistoryService {
    fn drop(&mut self) {
        if self.state.is_some() {
            for error in self.stop_all() { eprintln!("{error}"); }
        }
    }
}
