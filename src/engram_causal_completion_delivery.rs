// Automatic remote verification uses the destination's own ordinary Engram
// connection. Original mailbox send rows own all durable request/receipt state;
// the joined federation supervisor owns only this disposable scheduling worker.

impl AppState {
    fn causal_test_admission_target(
        &self,
        delivery: &CausalTestCompletionDelivery,
    ) -> Option<EngramBindingTarget> {
        let scope = delivery.scope.as_ref()?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = federation_mailbox_root_locked(
            &inner,
            &delivery.selector.payload.requester.native_session_id,
        )
        .ok()?;
        if record.federation_native_creation_id
            != delivery.selector.payload.requester.native_creation_id
            || inner.federation.local.as_ref() != Some(&delivery.selector.payload.requester.host)
        {
            return None;
        }
        let target = Self::engram_source_root_target_locked(
            &inner,
            &delivery.selector.payload.requester.native_session_id,
        )
        .ok()
        .flatten()?;
        if target.project_id != scope.project_id
            || target.settings.authority_store_key.as_ref() != Some(&scope.store)
            || target.connection.session_id != delivery.selector.payload.requester.native_session_id
        {
            return None;
        }
        Some(target)
    }

    fn settle_causal_test_completion(&self, delivery: &CausalTestCompletionDelivery) -> Result<()> {
        let store = &self.mailbox_store;
        let selector = &delivery.selector;
        let scope = delivery.scope.as_ref();
        let original = store.causal_test_settlement(selector, scope)?;
        let mut saved = if let Some(original) = original {
            original
        } else {
            let original = CausalTestSettlement::new(delivery);
            // No connection or write has been attempted. Readback/COMMIT of the
            // exact original body must succeed before the first external write.
            store.save_causal_test_settlement(selector, scope, None, &original)?;
            for (index, admission) in original.admissions.iter().enumerate() {
                if let Some(refusal) = &admission.refusal {
                    eprintln!(
                        "engram> remote run={} target={} admission=refused ({refusal})",
                        original.completion.run_id,
                        index + 1
                    );
                }
            }
            original
        };
        // Local exact replay always precedes a current-authority lookup.
        if saved.complete() {
            return Ok(());
        }
        // An authenticated conflicting later snapshot cannot replace the frozen
        // native facts, key or any receipt. Engram owns result-conflict admission.
        if saved.completion != delivery.completion {
            bail!("conflicting original causal completion; retained operation is unchanged");
        }
        for index in 0..saved.admissions.len() {
            if self.federation_executor_wake_signal.is_stopping() {
                break;
            }
            if saved.admissions[index].receipt.is_some()
                || saved.admissions[index].refusal.is_some()
            {
                continue;
            }
            let before = saved.clone();
            let Some(target) = self.causal_test_admission_target(delivery) else {
                saved.admissions[index].pending =
                    Some("original destination connection unavailable".into());
                if before != saved {
                    store.save_causal_test_settlement(selector, scope, Some(&before), &saved)?;
                    eprintln!(
                        "engram> remote run={} target={} admission=pending (original destination unavailable)",
                        saved.completion.run_id,
                        index + 1
                    );
                }
                continue;
            };
            if saved.admissions[index]
                .connection
                .as_ref()
                .is_some_and(|original| original != &target.connection)
            {
                saved.admissions[index].pending =
                    Some("original destination connection changed; no route replacement".into());
                if before != saved {
                    store.save_causal_test_settlement(selector, scope, Some(&before), &saved)?;
                    eprintln!(
                        "engram> remote run={} target={} admission=pending (original connection changed)",
                        saved.completion.run_id,
                        index + 1
                    );
                }
                continue;
            }
            if saved.admissions[index].connection.is_none() {
                saved.admissions[index].connection = Some(target.connection.clone());
                store.save_causal_test_settlement(selector, scope, Some(&before), &saved)?;
            }
            let before = saved.clone();
            let admission = &mut saved.admissions[index];
            let request = admission
                .request
                .as_ref()
                .expect("validated original request");
            let reply = (|| {
                let scope = scope.expect("validated destination scope");
                target.adapter.read_work_session(
                    &target.connection,
                    &scope.store,
                    target.settings.call_timeout(),
                )?;
                if !target
                    .adapter
                    .remote_verification_record_route_support(&target.connection)
                    .is_supported()
                {
                    return Err(EngramTransportError::protocol(
                        "remote verification capability unavailable",
                    ));
                }
                // This snapshot revalidation fences settings, actor, store and
                // runtime after capability I/O, without refreshing the captured
                // claim/root association. Engram judges NEW association currency.
                let now = self.causal_test_admission_target(delivery).ok_or_else(|| {
                    EngramTransportError::protocol("original destination unavailable")
                })?;
                if now.connection != target.connection
                    || !now.settings.same_work_service_settings(&target.settings)
                    || now.runtime_snapshot != target.runtime_snapshot
                {
                    return Err(EngramTransportError::protocol(
                        "original destination changed before write",
                    ));
                }
                target.adapter.request(
                    &target.connection,
                    &request.clone().into_control_request(),
                    target.settings.call_timeout(),
                )
            })();
            match reply {
                Ok(value) => match request.receipt(value.clone()) {
                    Ok(_) => {
                        admission.receipt = Some(value);
                        admission.pending = None;
                    }
                    Err(_) => {
                        admission.pending = Some("unconfirmed malformed remote receipt".into());
                    }
                },
                Err(error)
                    if error.kind == EngramTransportErrorKind::Remote
                        && matches!(
                            error.code.as_deref(),
                            Some("invalid_request" | "control_operation_idempotency_conflict")
                        ) =>
                {
                    admission.refusal = error.code.clone();
                    admission.pending = None;
                }
                Err(error) => {
                    // Only a bounded code is made visible; no received text,
                    // private association, command line or path reaches logs.
                    admission.pending = Some(
                        match error.kind {
                            EngramTransportErrorKind::Remote => {
                                "remote verification deferred; retry original operation"
                            }
                            _ => "remote verification unavailable; original operation unconfirmed",
                        }
                        .into(),
                    );
                }
            }
            // Persist the actual ORIGINAL receipt before ending retry custody.
            // A crash before this COMMIT simply retries the same body/key.
            store.save_causal_test_settlement(selector, scope, Some(&before), &saved)?;
            if before != saved {
                let admission = &saved.admissions[index];
                let decision = admission
                    .receipt
                    .as_ref()
                    .and_then(|value| request_qualification_status(value))
                    .unwrap_or_else(|| {
                        if admission.refusal.is_some() {
                            "refused"
                        } else {
                            "pending"
                        }
                    });
                eprintln!(
                    "engram> remote run={} target={} admission={decision}",
                    saved.completion.run_id,
                    index + 1
                );
            }
            // At most one external admission per completion per bounded pass.
            // Remaining associations retain their original pending custody.
            break;
        }
        Ok(())
    }
}

fn request_qualification_status(value: &Value) -> Option<&'static str> {
    let qualification: engram_remote_verification::Qualification =
        serde_json::from_value(value.get("qualification")?.clone()).ok()?;
    Some(if qualification.credited {
        "credited"
    } else {
        "recorded-uncredited"
    })
}

#[derive(Default)]
struct CausalTestCompletionRecovery {
    task: Option<tokio::task::JoinHandle<()>>,
    observed_kick: u64,
    fallback_owed: bool,
}

impl CausalTestCompletionRecovery {
    async fn tick(&mut self, app: &AppState, fallback: bool) {
        // A periodic tick remains owed while the joined worker is busy.
        self.fallback_owed |= fallback;
        if self.task.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        if let Some(task) = self.task.take() {
            if task.await.is_err() {
                eprintln!("causal completion recovery: worker_join_failed; pending rows retained");
            }
        }
        if app.federation_executor_wake_signal.is_stopping() {
            return;
        }
        let generation = app
            .federation_executor_wake_signal
            .generation
            .load(Ordering::SeqCst);
        if !self.fallback_owed && generation == self.observed_kick {
            return;
        }
        self.observed_kick = generation;
        let fallback = std::mem::take(&mut self.fallback_owed);
        let app = app.clone();
        self.task = Some(tokio::spawn(async move {
            match app.read_ready_causal_test_completions(fallback).await {
                Ok(deliveries) => {
                    for delivery in deliveries {
                        if app.federation_executor_wake_signal.is_stopping() {
                            break;
                        }
                        let worker = app.clone();
                        let result = run_blocking_api(move || {
                            let result = worker.settle_causal_test_completion(&delivery);
                            let reason = if result.is_err() {
                                CausalPendingReason::AdmissionFailed
                            } else {
                                CausalPendingReason::AdmissionPending
                            };
                            worker
                                .mailbox_store
                                .note_causal_pending_for_selector(
                                    &delivery.selector,
                                    delivery.pending.intents,
                                    reason,
                                )
                                .map_err(mailbox_api_error)?;
                            result.map_err(|_| {
                                ApiError::conflict("causal remote admission remains pending")
                            })
                        })
                        .await;
                        if result.is_err() {
                            eprintln!(
                                "causal completion recovery: admission_failed; inspect durable pending diagnostics"
                            );
                        }
                    }
                }
                Err(_) => eprintln!(
                    "causal completion recovery: pending_read_failed; original rows retained"
                ),
            }
            // Requester hints take priority over bounded outbound retries.
            app.notify_causal_test_completions().await;
            // A hint arriving during this joined worker retains its key and
            // a generation kick. More than one bounded batch needs a new kick.
            let pending = !app
                .federation_executor_wake_signal
                .causal_ready
                .lock()
                .expect("ready mutex poisoned")
                .pending
                .is_empty();
            if pending {
                app.federation_executor_wake_signal.kick();
            } else {
                app.federation_executor_wake_signal.notify.notify_one();
            }
        }));
    }

    async fn shutdown(self) {
        if let Some(task) = self.task {
            let _ = task.await;
        }
    }
}
