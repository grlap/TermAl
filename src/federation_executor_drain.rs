// One bounded central dispatch at a time. Lost responses retry the same
// durable operation; transport reconnection never mints application identity.
#[derive(Default)]
struct FederationExecutorWakeSignal {
    stopping: AtomicBool,
    receives: Mutex<usize>,
    receives_done: std::sync::Condvar,
    recovery_pass: Mutex<()>,
    activation_after: Mutex<Option<(String, String, String)>>,
    #[cfg(test)]
    lifecycle_hook: Mutex<Option<FederationExecutorLifecycleHook>>,
    // Holds the next outcome poll before its RPC, once, as a stalled receiver
    // would: signals entry, then waits for release.
    #[cfg(test)]
    outcome_poll_hold: Mutex<Option<(std::sync::mpsc::Sender<()>, Arc<tokio::sync::Notify>)>>,
    // On a receiver: holds its next outcome answer's blocking read, once, as
    // a slow read would: signals entry, then waits until released or dropped.
    #[cfg(test)]
    outcome_answer_hold: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    generation: AtomicU64,
    notify: tokio::sync::Notify,
    live: Mutex<BTreeMap<(String, String, String), FederationExecutorLiveActivation>>,
    activation_work: Mutex<BTreeMap<(String, String, String), FederationExecutorActivationWork>>,
}

struct FederationExecutorLiveActivation {
    server_instance_id: String,
    stop_generation: u64,
    pause_epoch: u64,
}

impl FederationExecutorWakeSignal {
    fn kick(&self) {
        if self.is_stopping() { return; }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.notify.notify_one();
    }
}

#[derive(Default)]
struct FederationExecutorWakeDrain {
    after: String,
    observed_kick: u64,
    pending: Option<tokio::sync::oneshot::Receiver<(String, bool)>>,
    task: Option<tokio::task::JoinHandle<()>>,
    // Outcome polling has its own single owned pass, never on the dispatch
    // path: a slow receiver answer cannot delay the next wake dispatch.
    outcome_poll: Option<tokio::task::JoinHandle<()>>,
}

impl FederationExecutorWakeDrain {
    fn tick(&mut self, app: &AppState) {
        self.tick_inner(app, true);
    }

    fn tick_inner(&mut self, app: &AppState, fallback: bool) {
        if app.federation_executor_wake_signal.is_stopping() { return; }
        // Outcomes are due by backoff, not by appends: the fallback tick polls.
        if fallback { self.start_outcome_poll(app); }
        let mut more = false;
        if let Some(pending) = &mut self.pending {
            match pending.try_recv() {
                Ok((after, remaining)) => { self.after = after; more = remaining; }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {}
            }
            self.pending = None;
        }
        let generation = app.federation_executor_wake_signal.generation.load(Ordering::SeqCst);
        if !fallback && !more && generation == self.observed_kick { return; }
        {
            let inner = app.inner.lock().expect("state mutex poisoned");
            if federation_mailbox_central_locked(&inner).is_err() { return; }
        }
        let (send, receive) = tokio::sync::oneshot::channel();
        self.observed_kick = generation;
        let app = app.clone();
        let after = self.after.clone();
        self.task = Some(tokio::spawn(async move {
            let next = app.drain_federation_executor_wakes(&after, 16).await;
            let _ = send.send(next);
            // Completion wakes only this owner. It starts another batch only
            // for progress with remaining work, or a newer append/reconnect.
            app.federation_executor_wake_signal.notify.notify_one();
        }));
        self.pending = Some(receive);
    }

    /// Starts one pass of settled wakes' outcome polling unless one runs.
    fn start_outcome_poll(&mut self, app: &AppState) {
        if self.outcome_poll.as_ref().is_some_and(|task| !task.is_finished()) { return; }
        let app = app.clone();
        self.outcome_poll = Some(tokio::spawn(async move { app.poll_federation_executor_outcomes().await; }));
    }

    async fn shutdown(self) {
        if let Some(task) = self.task {
            if let Err(error) = task.await { eprintln!("remote wake drain failed: {error}"); }
        }
        if let Some(task) = self.outcome_poll {
            if let Err(error) = task.await { eprintln!("remote wake outcome polling failed: {error}"); }
        }
    }
}

impl AppState {
    #[cfg(test)]
    async fn drain_one_federation_executor_wake(&self, after: &str) -> String {
        self.drain_federation_executor_wakes(after, 1).await.0
    }

    async fn drain_federation_executor_wakes(&self, after: &str, batch: usize) -> (String, bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let requests = loop {
            if self.federation_executor_wake_signal.is_stopping() { return (after.to_owned(), false); }
            let app = self.clone();
            let page = run_blocking_api(move || {
                app.refresh_federation_executor_reservation_domain()?;
                let before = app.mailbox_store.executor_reservations.lock()
                    .expect("executor reservation mutex poisoned").scan_after;
                match app.mailbox_store.reserve_federation_executor_wakes() {
                    Ok(requests) => Ok(Some(requests)),
                    Err(error) => {
                        let cache = app.mailbox_store.executor_reservations.lock()
                            .expect("executor reservation mutex poisoned");
                        let scan_progress = error.downcast_ref::<MailboxStoreError>()
                            .is_some_and(|error| error.kind == MailboxStoreErrorKind::Capacity)
                            && cache.scan_after > before && !cache.exhausted
                            && cache.window.len() < FEDERATION_EXECUTOR_RESERVATION_WINDOW;
                        if scan_progress { Ok(None) } else { Err(mailbox_api_error(error)) }
                    }
                }
            }).await;
            match page {
                Ok(Some(requests)) => break requests,
                Ok(None) if std::time::Instant::now() < deadline => tokio::task::yield_now().await,
                Ok(None) => return (after.to_owned(), false),
                Err(error) => {
                    eprintln!("remote wake reservation unavailable ({})", error.status);
                    return (after.to_owned(), false);
                }
            }
        };
        let start = requests.iter().position(|request| request.operation_key == after)
            .map_or(0, |index| index + 1);
        let mut next = after.to_owned();
        let mut attempted = 0;
        let mut scanned = 0;
        let mut progressed = false;
        for request in requests.iter().cycle().skip(start).take(requests.len()) {
            if self.federation_executor_wake_signal.is_stopping() { break; }
            if std::time::Instant::now() >= deadline { break; }
            scanned += 1;
            next = request.operation_key.clone();
            if self.federation_peer_handle(&request.target.identity.host_id)
                .is_none_or(|peer| peer.channel.current_lease().is_err()) { continue; }
            attempted += 1;
            match self.dispatch_federation_executor_wake(request.clone()).await {
                Ok(()) => progressed = true,
                Err(error) => eprintln!("remote wake remains pending ({})", error.status),
            }
            if attempted >= batch || std::time::Instant::now() >= deadline { break; }
        }
        (next, progressed && scanned < requests.len())
    }

    async fn dispatch_federation_executor_wake(&self, request: FederationExecutorWakeRequest)
        -> Result<(), ApiError>
    {
        let peer = self.federation_peer_handle(&request.target.identity.host_id)
            .ok_or_else(federation_transport_unavailable)?;
        let lease = peer.channel.current_lease()?;
        let basis = lease.with_current(self, |_, basis| Ok(basis.clone()))?;
        let operation = request.operation(basis.enrollment_generation)?;
        let app = self.clone();
        let reserved_request = request.clone();
        let reserved_operation = operation.clone();
        let reserved_lease = lease.clone();
        run_blocking_api(move || {
            app.refresh_federation_executor_reservation_domain()?;
            let authority = FederationExecutorMirrorAuthority {
                app: &app, operation: &reserved_operation, lease: &reserved_lease,
            };
            app.mailbox_store.reserve_exact_federation_executor_wake(&reserved_request,
                || authority.lock()).map_err(mailbox_api_error)
        }).await?;
        let receipt: FederationExecutorReceipt = peer.rpc.request(
            FederationRpcRoute::ExecutorWake(lease.clone(), request.clone())).await?;
        let app = self.clone();
        run_blocking_api(move || {
            let authority = FederationExecutorMirrorAuthority { app: &app, operation: &operation, lease: &lease };
            app.mailbox_store.settle_federation_executor_mirror(&request, &operation, &receipt,
                || authority.lock()).map_err(mailbox_api_error)
        }).await
    }
}
