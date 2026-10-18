// Owns the controlled onboarding commit-cut writer, without rotation fixtures.
// Does not implement production persistence. Split from federation_connect_writer.rs
// so onboarding controls retain only the writer they exercise.

use super::*;

pub(super) struct ControlledWriter {
    tx: mpsc::Sender<PersistRequest>,
    pub(super) fences: tokio::sync::mpsc::UnboundedReceiver<Box<PersistFence>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ControlledWriter {
    pub(super) fn attach(app: &mut AppState) -> Self {
        app.shutdown_persist_blocking();
        let (tx, rx) = mpsc::channel();
        app.persist_tx = tx.clone();
        app.persist_worker_alive.store(true, Ordering::SeqCst);
        let (send, fences) = tokio::sync::mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                if let PersistRequest::Fence(fence) = request {
                    let _ = send.send(fence);
                }
            }
        });
        Self {
            tx,
            fences,
            stop,
            thread: Some(thread),
        }
    }

    pub(super) async fn take(&mut self) -> Box<PersistFence> {
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, self.fences.recv())
            .await
            .unwrap()
            .expect("federation fence")
    }

    pub(super) fn commit(app: &AppState, fence: &PersistFence) {
        let state = PersistedState::from_inner(&app.inner.lock().unwrap());
        persist_persisted_state_to_sqlite(app.persistence_path.as_path(), &state).unwrap();
        let connection = rusqlite::Connection::open(app.persistence_path.as_path()).unwrap();
        assert!(fence.target.is_already_durable(&connection).unwrap());
        fence.finish(Ok(()));
    }
}

impl Drop for ControlledWriter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(PersistRequest::Delta);
        self.thread.take().unwrap().join().unwrap();
    }
}
