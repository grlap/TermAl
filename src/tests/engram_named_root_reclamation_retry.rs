// Pins retry progress independently of a retained diagnostic from an earlier read.
use super::*;

mod scheduling {
    include!("engram_named_root_reclamation_scheduling.rs");
}

struct ColdStartRootReader {
    process: ProcessEngramControlTransport,
    control: Arc<ScriptedEngramControlTransport>,
    home: PathBuf,
    attempts: Mutex<usize>,
}

impl EngramControlTransport for ColdStartRootReader {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        if matches!(request, EngramControlRequest::NamedRootRead { .. }) {
            *self.attempts.lock().unwrap() += 1;
            let prepared = self.process.processes.lock().unwrap()
                .get(&connection.session_id).cloned();
            if let Some(prepared) = prepared {
                assert!(prepared.process.try_wait().unwrap().is_none());
                assert!(!prepared.worker_finished.load(std::sync::atomic::Ordering::Acquire));
                // Retention is the real-process witness. Reclamation consumes
                // a scripted proof so its fixture's 250 ms bound never measures
                // OS scheduling or the control fixture's response latency.
                return self.control.request(connection, request, timeout);
            }
            let mut connection = connection.clone();
            connection.binary_path = real_engram_control_fixture_path();
            connection.home = self.home.clone();
            // A cold startup deterministically spends this call's budget.
            return self.process.request(&connection, request, Duration::ZERO);
        }
        self.control.request(connection, request, timeout)
    }

    fn shutdown_session(&self, session: &str) {
        self.process.shutdown_session(session);
        self.control.shutdown_session(session);
    }
}

#[test]
fn named_root_reclamation_startup_discard_blocks_completed_selection() {
    let (claimed, entry) = settled_root("reclaim-cold-process", "completed");
    let home = claimed.state.test_temp_root.as_ref().unwrap().path().to_path_buf();
    let proof = claimed.transport.named_roots.lock().unwrap().as_ref().unwrap()
        .root_reads[&entry.claim_id].clone();
    fs::write(home.join("root-lifecycle-read.json"), serde_json::to_vec(&proof).unwrap()).unwrap();
    let transport = Arc::new(ColdStartRootReader {
        process: ProcessEngramControlTransport::with_startup_handshake(
            home.clone(), "termal-engram-control-fixture-ready", DEADLOCK_GUARD),
        control: claimed.transport.clone(),
        home,
        attempts: Mutex::new(0),
    });
    install_control_only_transport(&claimed.state, transport.clone());
    let clock = claimed.state.engram_budget_clock();
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    assert_root_retained(&claimed, &entry);
    assert_eq!(engram_named_root_capacity(&claimed.state.inner.lock().unwrap()), 1);
    clock.advance(ENGRAM_ROOT_RECLAMATION_COOLDOWN);
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    assert_eq!(*transport.attempts.lock().unwrap(), 2);
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!inner.engram_work_source_roots.contains(&entry),
        "a completed associated selection must recover after a cold-start-only failure");
    assert_eq!(engram_named_root_capacity(&inner), 0);
    assert!(inner.engram_work_naming_history[0].retirements[0].published);
}

struct StartupExhaustedRootReader {
    control: Arc<ScriptedEngramControlTransport>,
    clock: EngramBudgetClock,
    read_budgets: Mutex<Vec<Duration>>,
}

impl EngramControlTransport for StartupExhaustedRootReader {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        if matches!(request, EngramControlRequest::NamedRootRead { .. }) {
            let mut budgets = self.read_budgets.lock().unwrap();
            budgets.push(timeout);
            if budgets.len() == 1 {
                self.clock.advance(timeout);
                return Err(EngramTransportError::deadline(
                    "Engram admission budget exhausted during process startup",
                ));
            }
        }
        self.control.request(connection, request, timeout)
    }

    fn shutdown_session(&self, session: &str) {
        self.control.shutdown_session(session);
    }
}

#[test]
fn named_root_reclamation_startup_failure_retries_with_fresh_budget_after_cooldown() {
    let (claimed, entry) = settled_root("reclaim-startup-retry", "completed");
    let clock = claimed.state.engram_budget_clock();
    let transport = Arc::new(StartupExhaustedRootReader {
        control: claimed.transport.clone(),
        clock: clock.clone(),
        read_budgets: Mutex::new(Vec::new()),
    });
    install_control_only_transport(&claimed.state, transport.clone());
    let binding_before = claimed.record(|record| record.engram.work_binding.clone());
    claimed.state.run_engram_root_reclamation_pass(reclamation_plan(&claimed));
    assert_root_retained(&claimed, &entry);
    {
        let inner = claimed.state.inner.lock().unwrap();
        assert_eq!(engram_named_root_capacity(&inner), 1);
        assert!(inner.engram_work_naming_history[0].retirements.is_empty());
        assert!(inner.engram_work_naming_history[0].recovery_reason.as_ref().unwrap()
            .contains("budget exhausted during process startup"));
    }
    assert!(reclamation_plan(&claimed).is_empty(), "the failed selection must cool down");
    clock.advance(ENGRAM_ROOT_RECLAMATION_COOLDOWN);
    let later = reclamation_plan(&claimed);
    assert_eq!(later.len(), 1);
    claimed.state.run_engram_root_reclamation_pass(later);
    let budgets = transport.read_budgets.lock().unwrap();
    assert_eq!(budgets.len(), 2);
    assert!(!budgets[0].is_zero());
    assert_eq!(budgets[1], budgets[0], "a later pass has its own full read budget");
    let inner = claimed.state.inner.lock().unwrap();
    assert!(!inner.engram_work_source_roots.contains(&entry));
    assert_eq!(engram_named_root_capacity(&inner), 0);
    assert_eq!(inner.engram_work_naming_history[0].retirements.len(), 1);
    assert!(inner.engram_work_naming_history[0].retirements[0].published);
    drop(inner);
    assert_eq!(claimed.record(|record| record.engram.work_binding.clone()), binding_before);
    assert!(matches!(claimed.runtime_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
}

#[test]
fn named_root_reclamation_missing_historical_association_is_unknown_without_guessing() {
    let (claimed, entry) = settled_root("reclaim-no-association", "completed");
    {
        let mut inner = claimed.state.inner.lock().unwrap();
        inner.engram_named_root_journal[0].read_binding = None;
        // This is the legacy shape: no exact run association survives locally.
        inner.engram_work_naming_history.clear();
        engram_record_naming_history(&mut inner, &entry.store, &entry.work_id, entry.generation);
    }
    let requests_before = claimed.transport.requests().len();
    assert!(reclamation_plan(&claimed).is_empty());
    let error = claimed.state.recover_one_engram_root_until(
        &entry, &EngramRootReclamationRoute::Unfocused,
        claimed.state.engram_budget_clock().now() + ENGRAM_ROOT_RECLAMATION_BUDGET,
    ).unwrap_err();
    assert!(error.message.contains("association"));
    assert_eq!(claimed.transport.requests().len(), requests_before);
    assert_root_retained(&claimed, &entry);
    assert_eq!(engram_named_root_capacity(&claimed.state.inner.lock().unwrap()), 1);
}
