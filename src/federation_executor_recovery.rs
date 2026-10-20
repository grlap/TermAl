// Owns bounded local inbox recovery, never global queue resume or host selection.
// A pass only materializes proven associations; runtime hand-offs keep their
// existing uncertainty and require separate positive settlement evidence.

#[derive(Default)]
struct FederationExecutorRecovery {
    after: String,
    pending: Option<tokio::sync::oneshot::Receiver<String>>,
    task: Option<std::thread::JoinHandle<()>>,
}

impl AppState {
    fn recover_federation_executor_after_boot(&self) {
        // Register only this M4 step, not the unrelated post-listen owner.
        let Ok(_job) = self.federation_executor_wake_signal.begin_receive() else { return; };
        FederationExecutorRecovery::default().recover(self);
    }
}

impl FederationExecutorRecovery {
    async fn tick(&mut self, app: &AppState) {
        if let Some(pending) = &mut self.pending {
            match pending.try_recv() {
                Ok(after) => self.after = after,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {}
            }
            self.pending = None;
            self.join().await;
        }
        if app.federation_executor_wake_signal.is_stopping() { return; }
        {
            let inner = app.inner.lock().expect("state mutex poisoned");
            if !inner.preferences.federation.enabled
                || inner.preferences.federation.role != FederationRole::Satellite { return; }
        }
        let (send, receive) = tokio::sync::oneshot::channel();
        let app = app.clone();
        let after = self.after.clone();
        // One owned blocking pass at a time. An unresolved persistence wait
        // never blocks the federation supervisor or starts a second pass.
        match std::thread::Builder::new().name("federation-executor-recovery".into())
            .spawn(move || {
                let mut recovery = Self { after, ..Self::default() };
                recovery.recover(&app);
                let _ = send.send(recovery.after);
            }) {
            Ok(task) => { self.task = Some(task); self.pending = Some(receive); }
            Err(error) => eprintln!("executor recovery could not start: {error}"),
        }
    }

    async fn join(&mut self) {
        if let Some(task) = self.task.take() {
            match tokio::task::spawn_blocking(move || task.join()).await {
                Ok(Ok(())) => {}
                _ => eprintln!("executor recovery worker failed"),
            }
        }
    }

    async fn shutdown(mut self) { self.join().await; }

    fn recover(&mut self, app: &AppState) {
        // Boot preserves its FIFO position; all other passes share this
        // serialization before taking any store, authority or state lock.
        let _pass = app.federation_executor_wake_signal.recovery_pass.lock()
            .expect("executor recovery pass mutex poisoned");
        if app.federation_executor_wake_signal.is_stopping() { return; }
        let central = {
            let inner = app.inner.lock().expect("state mutex poisoned");
            if inner.preferences.federation.role != FederationRole::Satellite { return; }
            inner.preferences.federation.central.clone()
        };
        let Some(central) = central else { return; };
        let Ok(host) = app.federation_executor_host_id() else { return; };
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if app.federation_executor_wake_signal.is_stopping() { return; }
            let previous = self.after.clone();
            let rows = match app.mailbox_store.local_federation_executor_recovery(
                &central.host_id, &host, &self.after,
            ) {
                Ok(rows) => rows,
                Err(_) => return,
            };
            if rows.is_empty() { self.after.clear(); return; }
            let exhausted = rows.len() < 100;
            for row in rows {
                if app.federation_executor_wake_signal.is_stopping() { return; }
                #[cfg(test)]
                app.federation_executor_wake_signal.observe("receiverRecoveryRow");
                if row.operation.target_host_id == host && row.operation.command_kind == "wake" {
                    let result = if row.effect_state == "confirmed" && row.runtime_ack.is_some() {
                        app.federation_executor_wake_signal.live.lock().expect("live activation mutex poisoned")
                            .remove(&AppState::federation_executor_live_key(&row.operation));
                        app.reconcile_federation_executor_runtime_receipt(&row)
                    } else {
                        // No new activation is created by recovery. Only an
                        // exact same-process intent can survive a failed write.
                        app.queue_federation_executor_operation(&row.operation, false)
                    };
                    if let Err(error) = result {
                        if error.status != StatusCode::CONFLICT { return; }
                    }
                }
                self.after = row.operation.operation_key;
                if std::time::Instant::now() >= deadline { return; }
            }
            if exhausted { self.after.clear(); return; }
            if self.after <= previous || std::time::Instant::now() >= deadline { return; }
            std::thread::yield_now();
        }
    }
}

#[derive(Default)]
struct FederationExecutorSenderRecovery {
    after: String,
    pending: Option<tokio::sync::oneshot::Receiver<String>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl FederationExecutorSenderRecovery {
    async fn tick(&mut self, app: &AppState) {
        if let Some(pending) = &mut self.pending {
            match pending.try_recv() {
                Ok(after) => self.after = after,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {}
            }
            self.pending = None;
            self.join().await;
        }
        if app.federation_executor_wake_signal.is_stopping() { return; }
        {
            let inner = app.inner.lock().expect("state mutex poisoned");
            if !inner.preferences.federation.enabled || !inner.federation_durable
                || inner.federation_continuity_lost
                || inner.preferences.federation.role == FederationRole::Standalone { return; }
        }
        let (send, receive) = tokio::sync::oneshot::channel();
        let app = app.clone();
        let after = self.after.clone();
        self.task = Some(tokio::spawn(async move {
            let scan = app.clone();
            let rows = run_blocking_api(move || {
                let connection = scan.mailbox_store.connection().map_err(mailbox_api_error)?;
                let rows = connection.prepare(
                    "SELECT owner_session_id,idempotency_key FROM federation_mailbox_intents
                     WHERE state IN ('localIntentDurable','appendOutcomeUnknown')
                     ORDER BY owner_session_id,idempotency_key LIMIT 1000",
                ).map_err(|_| federation_transport_unavailable())?
                    .query_map([], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?)))
                    .map_err(|_| federation_transport_unavailable())?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|_| federation_transport_unavailable())?;
                Ok(rows)
            }).await;
            let mut next = after.clone();
            if let Ok(rows) = rows {
                if app.federation_executor_wake_signal.is_stopping() {
                    let _ = send.send(next);
                    return;
                }
                let identity = |row: &(String, String)| serde_json::to_string(row).expect("intent key serializes");
                let start = rows.iter().position(|row| identity(row) == after).map_or(0, |index| index + 1);
                if let Some((session, key)) = rows.get(start).or_else(|| rows.first()).cloned() {
                    #[cfg(test)]
                    app.federation_executor_wake_signal.observe("senderRecoveryRow");
                    next = identity(&(session.clone(), key.clone()));
                    // Reuse the exact route's owner, capability and canonical
                    // envelope fences. This never creates a new intent or key.
                    if let Err(error) = retry_retained_federation_intent(app, session, key).await {
                        eprintln!("retained sender intent remains held ({})", error.status);
                    }
                } else { next.clear(); }
            }
            let _ = send.send(next);
        }));
        self.pending = Some(receive);
    }

    async fn join(&mut self) {
        if let Some(task) = self.task.take() {
            if let Err(error) = task.await { eprintln!("sender recovery worker failed: {error}"); }
        }
    }

    async fn shutdown(mut self) { self.join().await; }
}
