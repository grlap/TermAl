// Simplified event-driven fixture worker reusing the production collector and
// SQLite fence components. The omission cut models a deferred session row.
use super::*;

pub(super) struct WriterControl {
    omit_binding: AtomicBool,
    observed: AtomicBool,
    events: Mutex<mpsc::Receiver<Value>>,
    publish: mpsc::Sender<Value>,
    retry: mpsc::Sender<PersistRequest>,
    central_store: Mutex<Option<PathBuf>>,
}

impl WriterControl {
    pub(super) fn set_central_store(&self, path: PathBuf) {
        *self.central_store.lock().unwrap() = Some(path);
    }
    pub(super) fn omit_next_binding(&self) {
        self.observed.store(false, Ordering::SeqCst);
        self.omit_binding.store(true, Ordering::SeqCst);
    }

    pub(super) fn wait_for_omitted_fence(&self) -> Value {
        self.events
            .lock()
            .unwrap()
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .unwrap()
    }

    pub(super) fn release(&self, app: &AppState) {
        self.omit_binding.store(false, Ordering::SeqCst);
        let _ = app.persist_tx.send(PersistRequest::Delta);
    }
}

pub(super) fn start_writer(app: &mut AppState) -> Arc<WriterControl> {
    let (tx, rx) = mpsc::channel();
    app.persist_tx = tx;
    let (publish, events) = mpsc::channel();
    let control = Arc::new(WriterControl {
        omit_binding: AtomicBool::new(false),
        observed: AtomicBool::new(false),
        events: Mutex::new(events),
        publish,
        retry: app.persist_tx.clone(),
        central_store: Mutex::new(None),
    });
    let observing = control.clone();
    let inner = app.inner.clone();
    let path = app.persistence_path.clone();
    let worker = std::thread::spawn(move || {
        let mut cache = SqlitePersistConnectionCache::new();
        let mut fences = PersistFenceBatch::default();
        let mut watermark = 0;
        while let Ok(request) = rx.recv() {
            let shutdown = matches!(fences.accept(request), PersistWorkerWaitOutcome::Shutdown);
            let shutdown = fences.drain(&rx, shutdown);
            let mut delta = collect_persist_delta_from_shared_state(&inner, watermark);
            let omit = observing.omit_binding.load(Ordering::SeqCst);
            let mut omitted = false;
            if omit {
                let ids = delta
                    .changed_sessions
                    .iter()
                    .filter(|row| row.federation_mailbox_binding.is_some())
                    .map(|row| row.session.id.clone())
                    .collect::<Vec<_>>();
                omitted = !ids.is_empty();
                delta
                    .changed_sessions
                    .retain(|row| !ids.contains(&row.session.id));
                delta.deferred_session_ids.extend(ids);
            }
            let target = fences.pending.first().map(|fence| fence.target.clone());
            let result = persist_delta_with_fences(&mut cache, &path, &delta, &mut fences);
            if result.is_ok() && !omit {
                watermark = delta.watermark;
            }
            if result.is_ok()
                && omitted
                && let Some(target) = target
                && !observing.observed.swap(true, Ordering::SeqCst)
            {
                let connection = cache.connection.as_ref().unwrap();
                let metadata = federation_persist_image(&delta.metadata);
                let stored: Option<String> = connection
                    .query_row(
                        "SELECT value_json FROM sessions WHERE id=?1",
                        [&delta.deferred_session_ids[0]],
                        |row| row.get(0),
                    )
                    .optional()
                    .unwrap();
                let persisted_binding = stored
                    .map(|raw| serde_json::from_str::<PersistedSessionRecord>(&raw).unwrap())
                    .and_then(|row| row.federation_mailbox_binding);
                let central_directory =
                    observing
                        .central_store
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|path| {
                            load_state(path)
                                .unwrap()
                                .unwrap()
                                .federation
                                .mailbox_directory
                                .len()
                        });
                let observation = json!({"metadataCommitted":federation_committed_image_matches(connection,&metadata).unwrap(),
                    "fenceAcknowledged":target.is_already_durable(connection).unwrap(),
                    "pending":fences.has_pending(),"omittedRows":delta.deferred_session_ids.len(),
                    "admissionHeld":!inner.lock().unwrap().federation_durable,
                    "persistedBinding":persisted_binding,"centralDirectoryEntries":central_directory});
                let _ = observing.publish.send(observation);
                // Release from the exact observed writer phase, not after
                // parent/child IPC and load_state round trips against a deadline.
                observing.omit_binding.store(false, Ordering::SeqCst);
                let _ = observing.retry.send(PersistRequest::Delta);
            }
            if shutdown {
                fences.fail(PersistFenceError::WorkerStopped);
                break;
            }
        }
    });
    *app.persist_thread_handle.lock().unwrap() = Some(worker);
    control
}
