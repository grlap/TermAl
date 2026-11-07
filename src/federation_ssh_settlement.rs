// Retains an exact outstanding SSH operation and any child needing another reap.
// Supervisor ticks retry only returned cleanup; a running OS call is never duplicated.
#[derive(Default)]
struct FederationOsAccount {
    unresolved: AtomicBool,
    flight: Mutex<Option<Arc<FederationOsFlight>>>,
}

#[derive(Default)]
struct FederationOsFlightState {
    clean: bool,
    running: bool,
    child: Option<Box<dyn FederationSshChild>>,
    stage: &'static str,
}

#[derive(Default)]
struct FederationOsFlight {
    state: Mutex<FederationOsFlightState>,
    #[cfg(test)]
    changed: tokio::sync::Notify,
}

impl FederationOsFlight {
    fn clean(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.clean = true;
        state.running = false;
        state.stage = "sshRecovered";
        #[cfg(test)]
        self.changed.notify_one();
    }

    fn cleanup(&self, mut child: Box<dyn FederationSshChild>) -> Result<(), ApiError> {
        // The original OS thread still owns the child while shutdown runs.
        // Err hands that SAME child back; it never proves reap.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| child.shutdown()))
            .unwrap_or_else(|_| Err(federation_transport_unavailable()));
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.running = false;
        if result.is_ok() {
            // Reap is already proven even if a diagnostic Drop panics.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(child)));
            state.clean = true;
            state.stage = "sshRecovered";
        } else {
            state.child = Some(child);
            state.stage = "sshCleanupRetry";
        }
        #[cfg(test)]
        self.changed.notify_one();
        result
    }

    fn retry(self: &Arc<Self>, reply: Option<tokio::sync::oneshot::Sender<Result<(), ApiError>>>) {
        {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.running || state.child.is_none() { return; }
            // Reserve before spawning. Spawn failure leaves the child here.
            state.running = true;
            state.stage = "sshCleanupPending";
        }
        let flight = self.clone();
        if std::thread::Builder::new().name("federation-ssh-cleanup".to_owned()).spawn(move || {
            let child = flight.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                .child.take().expect("reserved SSH cleanup child");
            let result = flight.cleanup(child);
            if let Some(reply) = reply { let _ = reply.send(result); }
        }).is_err() {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.running = false;
            state.stage = "sshCleanupRetry";
            #[cfg(test)]
            self.changed.notify_one();
        }
    }
}

impl FederationOsAccount {
    fn begin(&self, stage: &'static str) -> Arc<FederationOsFlight> {
        let flight = Arc::new(FederationOsFlight { state: Mutex::new(FederationOsFlightState {
            running: true, stage, ..Default::default()
        }), #[cfg(test)] changed: tokio::sync::Notify::new() });
        *self.flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(flight.clone());
        flight
    }

    fn pending(&self) -> bool { self.unresolved.load(Ordering::Acquire) }

    fn tick(&self) {
        if !self.pending() { return; }
        let flight = self.flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(flight) = flight {
            flight.retry(None);
            let current = self.flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if current.as_ref().is_some_and(|current| Arc::ptr_eq(current, &flight))
                && flight.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clean {
                self.unresolved.store(false, Ordering::Release);
            }
        }
    }

    fn stage(&self) -> &'static str {
        self.flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref()
            .map(|flight| flight.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).stage)
            .unwrap_or("runtimeQuarantine")
    }
}

struct FederationOsAwaitGuard {
    account: Arc<FederationOsAccount>,
    adopted: bool,
}
impl Drop for FederationOsAwaitGuard {
    fn drop(&mut self) {
        if !self.adopted { self.account.unresolved.store(true, Ordering::Release); }
    }
}

// A queued readiness notification alone never transfers ownership. The OS
// thread keeps the value until explicit adoption, or cleans it on receiver loss.
async fn federation_os_operation(
    operation: impl FnOnce(&mut Option<Box<dyn FederationSshChild>>) -> Result<(), ApiError> + Send + 'static,
    stop: Option<&mut tokio::sync::watch::Receiver<bool>>,
    account: Arc<FederationOsAccount>,
    timeout: Duration,
) -> Result<Box<dyn FederationSshChild>, ApiError> {
    let flight = account.begin("sshStartSettlement");
    let thread_flight = flight.clone();
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::Builder::new().name("federation-io".to_owned()).spawn(move || {
        // Ownership lives outside catch_unwind. The starter publishes the
        // exact child before any post-spawn setup that can panic or fail.
        let mut owned = None;
        let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&mut owned)))
            .unwrap_or_else(|_| Err(federation_transport_unavailable()));
        let value = match started {
            Ok(()) => owned.take().ok_or_else(|| { thread_flight.clean(); federation_transport_unavailable() }),
            Err(error) => {
                if let Some(child) = owned.take() { let _ = thread_flight.cleanup(child); }
                else { thread_flight.clean(); }
                Err(error)
            },
        };
        let value = Arc::new(Mutex::new(Some(value)));
        let (adopted, adoption) = std::sync::mpsc::channel();
        if send.send((value.clone(), adopted)).is_ok() && adoption.recv().is_ok() { return; }
        let value = value.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        match value {
            Some(Ok(child)) => { let _ = thread_flight.cleanup(child); },
            Some(Err(_)) => {},
            None => {},
        }
    }).map_err(|_| { flight.clean(); federation_transport_unavailable() })?;
    let mut guard = FederationOsAwaitGuard { account, adopted: false };
    let stopped = async {
        if let Some(stop) = stop { federation_wait_stop(stop).await; }
        else { std::future::pending::<()>().await; }
    };
    tokio::select! {
        result = receive => {
            let (value, adopted) = result.map_err(|_| federation_transport_unavailable())?;
            let result = value.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                .take().ok_or_else(federation_transport_unavailable)?;
            let clean = {
                let mut state = flight.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if result.is_ok() { state.running = false; }
                state.clean
            };
            // No await between taking the value and adoption acknowledgement.
            let _ = adopted.send(());
            guard.adopted = result.is_ok() || clean;
            result
        },
        _ = stopped => Err(federation_transport_unavailable()),
        _ = tokio::time::sleep(timeout) => Err(federation_transport_unavailable()),
    }
}

async fn federation_os_cleanup(
    child: Box<dyn FederationSshChild>, account: Arc<FederationOsAccount>, timeout: Duration,
) -> Result<(), ApiError> {
    let flight = account.begin("sshCleanupPending");
    {
        let mut state = flight.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.running = false;
        state.child = Some(child);
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    flight.retry(Some(send));
    let mut guard = FederationOsAwaitGuard { account, adopted: false };
    let result = tokio::select! {
        result = receive => result.map_err(|_| federation_transport_unavailable())?,
        _ = tokio::time::sleep(timeout) => return Err(federation_transport_unavailable()),
    };
    guard.adopted = result.is_ok();
    result
}
