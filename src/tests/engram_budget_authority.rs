// Exercises the shared authority clock through real binding and boot workers.
use super::*;

struct HeldBudgetBindingRead {
    control: Arc<ScriptedEngramControlTransport>,
    entered: std::sync::mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl EngramControlTransport for HeldBudgetBindingRead {
    fn shutdown_session(&self, session: &str) {
        self.control.shutdown_session(session);
    }
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        self.control.request(connection, request, timeout)
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.entered.send(()).unwrap();
        let (ready, changed) = &*self.release;
        let watchdog = std::time::Instant::now() + Duration::from_secs(30);
        let mut ready = ready.lock().unwrap();
        while !*ready {
            let left = watchdog.saturating_duration_since(std::time::Instant::now());
            assert!(!left.is_zero(), "binding fixture driver was not released");
            (ready, _) = changed.wait_timeout(ready, left).unwrap();
        }
        drop(ready);
        self.control
            .read_work_binding(connection, preference, timeout)
    }

    fn read_work_binding_for_boot(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.read_work_binding(connection, preference, timeout)
    }
}

#[test]
fn source_root_scripted_clock_covers_boot_worker_and_enclosing_batch() {
    for mode in ["positive", "worker-expiry", "batch-expiry"] {
        let label = format!("scripted-boot-{mode}");
        let claimed = ClaimedRoot::new_scripted(&label, vec![bind_reply("scripted-boot-token")]);
        prepare_confirmed_claimed_opening(&claimed, &label);
        let store = claimed_root_store(&claimed);
        let clock = claimed.state.engram_budget_clock();
        let initial_now = clock.now();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        install_control_only_transport(
            &claimed.state,
            Arc::new(HeldBudgetBindingRead {
                control: claimed.transport.clone(),
                entered: entered_tx,
                release: release.clone(),
            }),
        );
        let target = {
            let mut inner = claimed.state.inner.lock().unwrap();
            let index = inner.find_session_index(&claimed.session_id).unwrap();
            inner.sessions[index].engram_boot_recovery_pending = true;
            AppState::engram_binding_target_for_session_shape_locked(
                &inner,
                &claimed.session_id,
                true,
            )
            .unwrap()
            .unwrap()
        };
        let rpc_budget = target.settings.call_timeout();
        assert_eq!(target.budget_clock.now(), initial_now);
        let state = claimed.state.clone();
        let task = std::thread::spawn(move || {
            state.recover_prepared_engram_sessions_after_boot(EngramBootRecoveryPlan {
                targets: vec![target],
                budget: Duration::from_secs(2),
            });
        });
        entered_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        clock.wait_for_scripted_waiter();
        if mode == "batch-expiry" {
            clock.advance(Duration::from_secs(2) + Duration::from_millis(1));
            task.join()
                .expect("batch driver must stop while the worker is still held");
            assert!(claimed.record(|record| record.engram_boot_recovery_pending));
        } else {
            if mode == "worker-expiry" {
                clock.advance(rpc_budget + Duration::from_millis(1));
            } else {
                // A real scheduling delay exceeds both old real allowances;
                // the explicit logical budget has not advanced.
                std::thread::sleep(Duration::from_secs(2));
                assert_eq!(clock.now(), initial_now);
            }
            *release.0.lock().unwrap() = true;
            release.1.notify_all();
            task.join().unwrap();
        }
        if mode == "batch-expiry" {
            *release.0.lock().unwrap() = true;
            release.1.notify_all();
            let guard = phase_sync::PollGuard::new();
            while claimed.record(|record| record.engram_boot_recovery_pending) {
                guard.wait(format_args!(
                    "expired boot worker must finish its late refusal"
                ));
            }
        }
        let requests = claimed.transport.requests();
        if mode == "positive" {
            assert!(
                requests
                    .iter()
                    .any(|request| request.request["operation"] == "session_bind")
            );
            assert!(
                requests
                    .iter()
                    .any(|request| request.request["operation"] == "named_root_read")
            );
            let inner = claimed.state.inner.lock().unwrap();
            let work = inner.engram_work_naming_history[0].work_id.clone();
            assert!(!engram_authority_work_unresolved(&inner, &store, &work));
        } else {
            assert!(
                requests.is_empty(),
                "expired original budget must forbid transport: {mode}"
            );
            assert!(claimed.record(|record| record.engram.routing_token.is_none()));
            let inner = claimed.state.inner.lock().unwrap();
            assert!(
                inner
                    .engram_work_naming_history
                    .iter()
                    .any(|history| history.store == store
                        && engram_authority_work_unresolved(&inner, &store, &history.work_id))
            );
        }
    }
}
