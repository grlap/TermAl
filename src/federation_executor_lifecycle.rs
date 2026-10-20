// Owns the boot-local stopping fence and outstanding blocking receive work.
// Dropping an HTTP waiter does not cancel a transaction or release its credit.

#[cfg(test)]
type FederationExecutorLifecycleHook = Arc<dyn Fn(&'static str) + Send + Sync>;

struct FederationExecutorReceiveJob {
    signal: Arc<FederationExecutorWakeSignal>,
}

impl Drop for FederationExecutorReceiveJob {
    fn drop(&mut self) {
        let mut count = self.signal.receives.lock().expect("executor receive counter poisoned");
        *count -= 1;
        if *count == 0 { self.signal.receives_done.notify_all(); }
    }
}

impl FederationExecutorWakeSignal {
    fn is_stopping(&self) -> bool { self.stopping.load(Ordering::Acquire) }

    fn stop(&self) {
        if !self.stopping.swap(true, Ordering::AcqRel) {
            #[cfg(test)]
            self.observe("stopping");
        }
        self.notify.notify_waiters();
    }

    fn check_receiving(&self) -> Result<()> {
        if self.is_stopping() {
            return Err(mailbox_store_error(MailboxStoreErrorKind::Retryable,
                "executor is stopping; receive was not committed"));
        }
        Ok(())
    }

    fn begin_receive(self: &Arc<Self>) -> Result<FederationExecutorReceiveJob, ApiError> {
        // Registration and the final zero-count observation share this mutex.
        // After stop, no new job can escape the shutdown join.
        let mut count = self.receives.lock().expect("executor receive counter poisoned");
        self.check_receiving().map_err(mailbox_api_error)?;
        *count += 1;
        Ok(FederationExecutorReceiveJob { signal: self.clone() })
    }

    async fn join_receives(self: &Arc<Self>) {
        let signal = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut count = signal.receives.lock().expect("executor receive counter poisoned");
            while *count != 0 {
                count = signal.receives_done.wait(count).expect("executor receive counter poisoned");
            }
        }).await.expect("executor receive join failed");
        #[cfg(test)]
        self.observe("receivesJoined");
    }

    #[cfg(test)]
    fn observe(&self, event: &'static str) {
        let hook = self.lifecycle_hook.lock().unwrap().clone();
        if let Some(hook) = hook { hook(event); }
    }
}

// A claimed entry restores its one moved credit on unwind. No second Arc to
// the admission permit is created, and no work lock is held during storage.
struct FederationExecutorActivationClaim<'a> {
    signal: &'a FederationExecutorWakeSignal,
    key: (String, String, String),
    permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    finished: bool,
}

impl FederationExecutorActivationClaim<'_> {
    fn finish(mut self, release: bool) {
        let mut work = self.signal.activation_work.lock().expect("activation work mutex poisoned");
        if release { work.remove(&self.key); }
        else if let Some(entry) = work.get_mut(&self.key) {
            entry.in_flight = false;
            entry.permit = self.permit.take();
        }
        self.finished = true;
        // The guard drops the remaining permit before provider activation.
    }
}

impl Drop for FederationExecutorActivationClaim<'_> {
    fn drop(&mut self) {
        if self.finished { return; }
        let mut work = self.signal.activation_work.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = work.get_mut(&self.key) {
            entry.in_flight = false;
            entry.permit = self.permit.take();
        }
    }
}
