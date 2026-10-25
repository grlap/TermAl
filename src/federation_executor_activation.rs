// Owns same-boot post-receive work. The supervisor joins the single bounded
// worker; the receive callback moves its existing admission credit, never
// cloning it or waiting for session persistence/provider work.

struct FederationExecutorReceiveGuard<'a> {
    authority: FederationMailboxAdmission<'a>,
    live: std::sync::MutexGuard<'a, BTreeMap<(String, String, String), FederationExecutorLiveActivation>>,
    work: std::sync::MutexGuard<'a, BTreeMap<(String, String, String), FederationExecutorActivationWork>>,
}

struct FederationExecutorActivationWork {
    operation: FederationExecutorOperation,
    permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    in_flight: bool,
}

impl AppState {
    fn capture_federation_executor_receive(
        &self, guard: &mut FederationExecutorReceiveGuard<'_>, record: &FederationExecutorRecord,
        permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    ) {
        let operation = &record.operation;
        if record.queue_entry_id.is_some() || record.effect_state != "not_started" { return; }
        let inner = &guard.authority._inner;
        let Some(index) = inner.find_session_index(&operation.session_id) else { return; };
        let session = &inner.sessions[index];
        let key = Self::federation_executor_live_key(operation);
        if guard.work.contains_key(&key) { return; }
        if !session.orchestrator_auto_dispatch_blocked && !session.runtime_stop_in_progress {
            guard.live.entry(key.clone()).or_insert_with(|| FederationExecutorLiveActivation {
                server_instance_id: self.server_instance_id.clone(),
                stop_generation: session.runtime_stop_generation, pause_epoch: session.auto_dispatch_pause_epoch,
            });
        }
        // A receive past writer admission still commits and captures intent.
        // Stop forbids scheduling that intent; the next boot recovers dormant.
        if self.federation_executor_wake_signal.is_stopping() { return; }
        guard.work.insert(key, FederationExecutorActivationWork {
            operation: operation.clone(), permit: Some(permit), in_flight: false });
        self.federation_executor_wake_signal.kick();
    }

    fn run_federation_executor_activation_batch(&self) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let signal = &*self.federation_executor_wake_signal;
        let mut attempted = BTreeSet::new();
        let mut progressed = false;
        for _ in 0..16 {
            if signal.is_stopping() { return false; }
            let after = signal.activation_after.lock().expect("activation cursor poisoned").clone();
            let work = {
                let mut work = signal.activation_work.lock().expect("activation work mutex poisoned");
                let eligible = |key: &&(String, String, String)| !attempted.contains(*key)
                    && work.get(*key).is_some_and(|entry| !entry.in_flight && entry.permit.is_some());
                let key = work.keys().filter(eligible)
                    .find(|key| after.as_ref().is_none_or(|after| *key > after))
                    .or_else(|| work.keys().find(eligible)).cloned();
                key.map(|key| {
                    let entry = work.get_mut(&key).expect("selected entry");
                    entry.in_flight = true;
                    let claim = FederationExecutorActivationClaim { signal, key,
                        permit: entry.permit.take(), finished: false };
                    (entry.operation.clone(), claim)
                })
            };
            let Some((operation, claim)) = work else { return false; };
            let key = claim.key.clone();
            attempted.insert(key.clone());
            *signal.activation_after.lock().expect("activation cursor poisoned") = Some(key.clone());
            #[cfg(test)]
            signal.observe("activationClaimed");
            let result = self.queue_federation_executor_operation_inner(&operation, false, false);
            let durable = {
                let connection = self.mailbox_store.connection();
                connection.and_then(|connection| federation_executor_read(&connection,
                    &operation.central_host_id, &operation.operation_key))
                    .ok().flatten().is_some_and(|row| row.queue_entry_id.is_some())
            };
            let terminal = result.as_ref().err().is_some_and(|error|
                error.kind == Some(ApiErrorKind::FederationExecutorAssociationRetired));
            // `retired` releases this work's claim. The same-boot activation
            // ends only for a permanent retirement or a settled association:
            // a hand-off record still being saved by another drive settles by
            // itself, and if it fails the head needs this activation again.
            let (retired, ends_activation) = if terminal { (true, true) } else {
                let inner = self.inner.lock().expect("state mutex poisoned");
                let permanent = federation_executor_operation_retired(&inner, &operation);
                let record = inner.find_session_index(&operation.session_id)
                    .map(|index| &inner.sessions[index]);
                let association = record.is_some_and(|record|
                    federation_executor_association_retired(record, &operation));
                let in_flight = record.is_some_and(|record|
                    federation_executor_hand_off_in_flight(record, &operation));
                (permanent || association, permanent || (association && !in_flight))
            };
            if ends_activation {
                signal.live.lock().expect("live activation mutex poisoned").remove(&key);
            }
            // Release the admission credit at durable queue ownership, before
            // provider work, or at an exact terminal refusal. Never replay it.
            claim.finish(durable || retired);
            progressed |= durable || retired;
            if durable && !retired { self.dispatch_federation_executor_live_activation(&operation); }
            if let Err(error) = result {
                eprintln!("received executor activation deferred ({}): {}", error.status, error.message);
            }
            if std::time::Instant::now() >= deadline { break; }
        }
        // Each eligible key is attempted only once. Retryable/Ok-but-not-durable
        // entries wait for a later durability kick or fallback, rather than spin.
        progressed && !signal.is_stopping() && signal.activation_work.lock()
            .expect("activation work mutex poisoned").iter().any(|(key, entry)|
                !attempted.contains(key) && !entry.in_flight && entry.permit.is_some())
    }
}

#[derive(Default)]
struct FederationExecutorActivationWorker {
    observed_kick: u64,
    pending: Option<tokio::sync::oneshot::Receiver<bool>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl FederationExecutorActivationWorker {
    async fn tick(&mut self, app: &AppState, fallback: bool) {
        if app.federation_executor_wake_signal.is_stopping() { return; }
        let mut more = false;
        if let Some(pending) = &mut self.pending {
            match pending.try_recv() {
                Ok(remaining) => more = remaining,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {}
            }
            self.pending = None;
            if let Some(task) = self.task.take() {
                if let Err(error) = task.await { eprintln!("executor activation worker failed: {error}"); }
            }
        }
        let generation = app.federation_executor_wake_signal.generation.load(Ordering::SeqCst);
        if !fallback && !more && generation == self.observed_kick { return; }
        if !app.federation_executor_wake_signal.activation_work.lock().expect("activation work mutex poisoned")
            .values().any(|intent| intent.permit.is_some() && !intent.in_flight) { return; }
        self.observed_kick = generation;
        let (send, receive) = tokio::sync::oneshot::channel();
        let app = app.clone();
        self.task = Some(tokio::spawn(async move {
            let worker = app.clone();
            let more = match run_blocking_api(move || Ok(worker.run_federation_executor_activation_batch())).await {
                Ok(more) => more,
                Err(error) => {
                    eprintln!("executor activation batch failed: {}", error.message);
                    false
                }
            };
            let _ = send.send(more);
            app.federation_executor_wake_signal.notify.notify_one();
        }));
        self.pending = Some(receive);
    }

    async fn shutdown(self, app: &AppState) {
        if let Some(task) = self.task {
            if let Err(error) = task.await { eprintln!("executor activation worker failed: {error}"); }
        }
        // Same-boot recovery retains intent, but shutdown admits no new work
        // and releases pending credits after the owned job has been joined.
        app.federation_executor_wake_signal.activation_work.lock().expect("activation work mutex poisoned").clear();
    }
}
