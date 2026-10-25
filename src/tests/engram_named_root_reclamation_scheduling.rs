// Owns the controlled warm-queue scheduling witness for reclamation fixtures.
// Does not alter process transport or reclamation policy. Split from the retry
// fixture's real-process timing assumption; uses only that fixture's state.
use super::*;

struct WarmRequestGate {
    reader: Arc<ColdStartRootReader>,
    session: String,
    release: Option<std::sync::mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<Option<(u32, bool)>>>,
}

impl WarmRequestGate {
    fn install(reader: Arc<ColdStartRootReader>, session: &str) -> Self {
        let original = reader.process.processes.lock().unwrap()[session].clone();
        let (requests, queued) = std::sync::mpsc::channel::<EngramProcessRequest>();
        let proxy = Arc::new(EngramControlProcess {
            config: original.config.clone(),
            process: original.process.clone(),
            process_tree: original.process_tree.clone(),
            requests,
            worker_finished: original.worker_finished.clone(),
        });
        let (release, released) = std::sync::mpsc::channel();
        let (ready, waiting) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            ready.send(()).unwrap();
            let Ok(request) = queued.recv() else { return None };
            let observed = (original.process.id(), original.process.try_wait().unwrap().is_none());
            // The caller must return before this queue can advance. There is
            // no sleep or CPU load, and the call retains its original budget.
            released.recv_timeout(DEADLOCK_GUARD).unwrap();
            drop(request);
            Some(observed)
        });
        let gate = Self {
            reader,
            session: session.to_owned(),
            release: Some(release),
            worker: Some(worker),
        };
        gate.reader.process.processes.lock().unwrap().insert(session.to_owned(), proxy);
        waiting.recv_timeout(DEADLOCK_GUARD).unwrap();
        gate
    }

    fn finish(mut self) -> Option<(u32, bool)> {
        let _ = self.release.take().unwrap().send(());
        self.reader.process.shutdown_session(&self.session);
        self.worker.take().unwrap().join().unwrap()
    }
}

impl Drop for WarmRequestGate {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() { let _ = release.send(()); }
        self.reader.process.shutdown_session(&self.session);
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

#[test]
fn named_root_reclamation_completed_selection_does_not_depend_on_warm_process_scheduling() {
    let (claimed, entry) = settled_root("reclaim-warm-scheduling", "completed");
    let home = claimed.state.test_temp_root.as_ref().unwrap().path().to_path_buf();
    let proof = claimed.transport.named_roots.lock().unwrap().as_ref().unwrap()
        .root_reads[&entry.claim_id].clone();
    fs::write(home.join("root-lifecycle-read.json"), serde_json::to_vec(&proof).unwrap()).unwrap();
    let reader = Arc::new(ColdStartRootReader {
        process: ProcessEngramControlTransport::with_startup_handshake(
            home.clone(), "termal-engram-control-fixture-ready", DEADLOCK_GUARD),
        control: claimed.transport.clone(), home, attempts: Mutex::new(0),
    });
    install_control_only_transport(&claimed.state, reader.clone());
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    assert_root_retained(&claimed, &entry);
    let gate = WarmRequestGate::install(reader.clone(), &claimed.session_id);
    claimed.state.engram_budget_clock().advance(ENGRAM_ROOT_RECLAMATION_COOLDOWN);
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    let (retained, reason, published) = {
        let inner = claimed.state.inner.lock().unwrap();
        let history = &inner.engram_work_naming_history[0];
        (inner.engram_work_source_roots.contains(&entry), history.recovery_reason.clone(),
            history.retirements.iter().any(|notice| notice.published))
    };
    let intercepted = gate.finish();
    assert_eq!(*reader.attempts.lock().unwrap(), 2);
    assert!(!retained, "a completed selection must not depend on a real warm fixture reply: {reason:?}");
    assert!(published);
    assert!(intercepted.is_none(), "the warm lifecycle proof must not enter the OS request queue");
}
