// Owns bounded retry of an unresolved federation persistence image.
// Does not mint authority, release reservations or own transports.
// Split from federation_runtime.rs when adding production durability recovery.

#[derive(Default)]
struct FederationDurabilityRecovery {
    task: Option<std::thread::JoinHandle<()>>,
    pending: Option<FederationDurabilityJob>,
    retry: FederationBackoff,
    next: Option<std::time::Instant>,
}

struct FederationDurabilityJob {
    result: tokio::sync::oneshot::Receiver<Result<(), ApiError>>,
    began: std::time::Instant,
    reported_hold: bool,
}

impl FederationDurabilityRecovery {
    // Polled by the one host supervisor. Never waits under a lock or blocks
    // reconciliation on disk IO. Shutdown joins the owned job before its final
    // persistence drain. An unresolved job is never replaced by a second writer.
    fn tick(&mut self, app: &AppState) {
        if app.federation_executor_wake_signal.is_stopping() { return; }
        let now = std::time::Instant::now();
        if let Some(job) = &mut self.pending {
            match job.result.try_recv() {
                Ok(Ok(())) => {
                    self.pending = None;
                    self.retry.reset();
                    self.next = None;
                }
                Ok(Err(error)) => {
                    self.failed(now, error.status);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.failed(now, StatusCode::SERVICE_UNAVAILABLE);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    if !job.reported_hold && now.duration_since(job.began) >= Duration::from_secs(5)
                    {
                        job.reported_hold = true;
                        eprintln!(
                            "federation persistence recovery unresolved; authority remains held"
                        );
                    }
                    return;
                }
            }
        }
        if let Some(task) = &self.task {
            if !task.is_finished() { return; }
            if self.task.take().expect("completed durability task").join().is_err() {
                eprintln!("federation persistence recovery worker failed");
            }
        }
        let held = {
            let inner = app.inner.lock().expect("state mutex poisoned");
            !inner.federation_durable
                && !inner.federation_continuity_lost
                && inner.federation.local.is_some()
        };
        if !held {
            self.next = None;
            self.retry.reset();
            return;
        }
        let Some(next) = self.next else {
            // Allow a normal in-progress write to settle before retrying its
            // exact image. This is persistence only, including disabled prefs.
            self.next = Some(now + self.retry.next_delay());
            return;
        };
        if now < next {
            return;
        }
        let (send, result) = tokio::sync::oneshot::channel();
        let app = app.clone();
        match std::thread::Builder::new()
            .name("federation-persist-recovery".to_owned())
            .spawn(move || {
                #[cfg(test)]
                app.federation_executor_wake_signal.observe("durabilityRecoveryJob");
                // The existing content fence rechecks the exact image after
                // durable readback. It does not reset continuity quarantine,
                // change epochs or release original operation reservations.
                let _ = send.send(app.reconcile_federation_durability());
            }) {
            Ok(task) => {
                self.task = Some(task);
                self.pending = Some(FederationDurabilityJob {
                    result,
                    began: now,
                    reported_hold: false,
                })
            }
            Err(_) => {
                self.failed(now, StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }

    async fn shutdown(mut self) {
        if let Some(task) = self.task.take() {
            match tokio::task::spawn_blocking(move || task.join()).await {
                Ok(Ok(())) => {}
                _ => eprintln!("federation persistence recovery worker failed during shutdown"),
            }
        }
    }

    fn failed(&mut self, now: std::time::Instant, status: StatusCode) {
        self.pending = None;
        let delay = self.retry.next_delay();
        self.next = Some(now + delay);
        self.retry.record_failure();
        // External error bodies and persistence details may contain private
        // data. Log only the typed disposition and bounded retry cadence.
        eprintln!(
            "federation persistence recovery failed ({status}); authority remains held; retry in {} ms",
            delay.as_millis()
        );
    }
}
