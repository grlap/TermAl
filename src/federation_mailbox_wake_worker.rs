// Owns bounded, post-commit delivery of central-native mailbox wake obligations.
// The federation supervisor joins this worker; HTTP never owns its lifetime.

#[derive(Default)]
struct FederationMailboxWakeWorker {
    after: i64,
    observed_kick: u64,
    pending: Option<tokio::sync::oneshot::Receiver<(i64, bool)>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl FederationMailboxWakeWorker {
    async fn tick(&mut self, app: &AppState, fallback: bool) {
        if app.federation_executor_wake_signal.is_stopping() {
            return;
        }
        let mut more = false;
        if let Some(pending) = &mut self.pending {
            match pending.try_recv() {
                Ok((after, remaining)) => {
                    self.after = after;
                    more = remaining;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {}
            }
            self.pending = None;
            if let Some(task) = self.task.take() {
                if let Err(error) = task.await {
                    eprintln!("native mailbox wake worker failed: {error}");
                }
            }
        }
        let generation = app.federation_executor_wake_signal.generation.load(Ordering::SeqCst);
        if !fallback && !more && generation == self.observed_kick {
            return;
        }
        {
            let inner = app.inner.lock().expect("state mutex poisoned");
            if federation_mailbox_central_locked(&inner).is_err() {
                return;
            }
        }
        self.observed_kick = generation;
        let (send, receive) = tokio::sync::oneshot::channel();
        let app = app.clone();
        let after = self.after;
        self.task = Some(tokio::spawn(async move {
            let worker = app.clone();
            let next = run_blocking_api(move || {
                Ok(worker.run_native_mailbox_wake_batch(after))
            })
            .await
            .unwrap_or((after, false));
            let _ = send.send(next);
            app.federation_executor_wake_signal.notify.notify_one();
        }));
        self.pending = Some(receive);
    }

    async fn shutdown(self) {
        if let Some(task) = self.task {
            if let Err(error) = task.await {
                eprintln!("native mailbox wake worker failed during shutdown: {error}");
            }
        }
    }
}

impl AppState {
    fn run_native_mailbox_wake_batch(&self, after: i64) -> (i64, bool) {
        let central = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            match federation_mailbox_central_locked(&inner) {
                Ok(central) => central,
                Err(_) => return (after, false),
            }
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let rows = match self.mailbox_store.native_mailbox_wake_page(after) {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("native mailbox wake scan deferred: {error:#}");
                return (after, false);
            }
        };
        let mut next = after;
        let mut processed = 0;
        for (rowid, message_id, encoded_binding, unfinished) in &rows {
            if self.federation_executor_wake_signal.is_stopping() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            next = *rowid;
            processed += 1;
            if !*unfinished {
                continue;
            }
            let binding: FederationMailboxBinding = match serde_json::from_str(encoded_binding) {
                Ok(binding) => binding,
                Err(_) => continue,
            };
            if binding.central != central || binding.identity.host_id != central.host_id {
                continue;
            }
            if let Err(error) = self.deliver_native_mailbox_wake(message_id, &binding) {
                eprintln!("native mailbox wake remains pending: {error:#}");
            }
        }
        if processed < rows.len() || rows.len() == 16 {
            (next, true)
        } else {
            // A later fallback retries held rows; it does not spin on them.
            (0, false)
        }
    }

    fn deliver_native_mailbox_wake(
        &self,
        message_id: &str,
        binding: &FederationMailboxBinding,
    ) -> Result<()> {
        let message = self.mailbox_store.read_message(&binding.participant_key(), message_id)?;
        #[cfg(test)]
        {
            let hook = self.mailbox_store.before_native_wake.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        if self.federation_executor_wake_signal.is_stopping() {
            return Ok(());
        }
        let wake = self.mailbox_store.unread_wakeup_for_mailbox_through(
            &binding.participant_key(), &message.mailbox_id, message.sequence,
        )?;
        let Some(wake) = wake else {
            // Already acknowledged mail cannot create a new continuation.
            self.mailbox_store.record_initial_dispatch_outcome(message_id, "durableButNotWoken")?;
            return Ok(());
        };
        let session = &binding.identity.session_id;
        let outcome = self.queue_mailbox_wakeups_for_binding_outcome(
            session, vec![wake], MailboxWakeupRecovery::NeverWoken, false, Some(binding),
        )?;
        if !outcome.accepted {
            return Ok(());
        }
        self.mailbox_store.record_initial_dispatch_outcome(
            message_id, self.mailbox_queued_wake_disposition(session),
        )?;
        if self.federation_executor_wake_signal.is_stopping() {
            return Ok(());
        }
        // Ordinary drain/delivery retains Stop, pause, authorization and
        // durability holds, and owns rejection recovery for this queued wake.
        if let Some(dispatch) = self.dispatch_next_queued_turn(session, false)? {
            let _ = deliver_turn_dispatch(self, dispatch);
        }
        Ok(())
    }
}

impl MailboxStore {
    fn native_mailbox_wake_pending(&self, message_id: &str) -> Result<bool> {
        let connection = self.connection()?;
        connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM federation_mailbox_wake_obligations w
             JOIN mailbox_messages m ON m.id=w.message_id
             WHERE w.message_id=?1 AND m.dispatch_outcome IS NULL)",
            [message_id], |row| row.get(0),
        ).map_err(Into::into)
    }

    fn native_mailbox_wake_page(&self, after: i64) -> Result<Vec<(i64, String, String, bool)>> {
        let Some(connection) = self.connection_if_enabled() else {
            return Ok(Vec::new());
        };
        let mut statement = connection.prepare(
            // Page physical history so sparse unfinished work cannot scan it
            // all while holding the connection in a single batch.
            "SELECT w.rowid, w.message_id, w.target_binding_json,
                    w.state='pending' AND m.dispatch_outcome IS NULL
             FROM federation_mailbox_wake_obligations w
             JOIN mailbox_messages m ON m.id=w.message_id
             WHERE w.rowid > ?1
             ORDER BY w.rowid LIMIT 16",
        )?;
        let rows = statement.query_map([after], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }
}
