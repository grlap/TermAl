// Automatic remote verification uses the destination's own ordinary Engram
// connection. Original mailbox send rows own all durable request/receipt state;
// the joined federation supervisor owns only this disposable scheduling worker.

use super::*;

enum CausalAdmissionFailure {
    Pending(CausalAdmissionCheck),
    Refused(&'static str),
}

/// A settlement step that failed after the completion was read. The reason
/// goes on the original row; the error goes only to the worker's log.
#[derive(Debug)]
pub(crate) struct CausalSettlementFailure {
    pub(crate) reason: CausalPendingReason,
    pub(crate) error: anyhow::Error,
}

impl CausalSettlementFailure {
    fn read(error: anyhow::Error) -> Self {
        Self {
            reason: CausalPendingReason::SettlementReadFailed,
            error,
        }
    }

    fn save(error: anyhow::Error) -> Self {
        Self {
            reason: CausalPendingReason::SettlementSaveFailed,
            error,
        }
    }
}

/// A closed class for a settlement error, never its text: decode messages
/// can quote stored values. SQLite errors keep their result code and JSON
/// errors their category.
fn settlement_error_class(error: &anyhow::Error) -> String {
    for cause in error.chain() {
        if let Some(sqlite) = cause.downcast_ref::<rusqlite::Error>() {
            return match sqlite.sqlite_error_code() {
                Some(code) => format!("sqlite:{code:?}"),
                None => "sqlite".to_owned(),
            };
        }
        if let Some(json) = cause.downcast_ref::<serde_json::Error>() {
            return format!("json:{:?}", json.classify());
        }
    }
    "other".to_owned()
}

/// A closed class for a worker that did not join, never its panic payload.
fn join_failure_class(error: &tokio::task::JoinError) -> &'static str {
    if error.is_panic() {
        "panic"
    } else if error.is_cancelled() {
        "cancelled"
    } else {
        "other"
    }
}

impl CausalAdmissionFailure {
    fn from_transport(error: EngramTransportError, capability_read: bool) -> Self {
        use CausalAdmissionCheck as Check;
        use EngramTransportErrorKind as Kind;
        if error.kind == Kind::Remote {
            match error.code.as_deref() {
                Some("invalid_request") => return Self::Refused("invalid_request"),
                Some("control_operation_idempotency_conflict") => {
                    return Self::Refused("control_operation_idempotency_conflict");
                }
                _ => {}
            }
        }
        Self::Pending(if capability_read {
            Check::CapabilityReadFailed
        } else {
            match error.kind {
                Kind::LocalState => Check::LocalConnectionUnavailable,
                Kind::Deadline => Check::AdmissionDeadline,
                Kind::Transport => Check::AdmissionTransportFailed,
                Kind::Protocol => Check::AdmissionProtocolFailed,
                Kind::Remote => Check::AdmissionDeferred,
                #[cfg(test)]
                Kind::Backoff => Check::AdmissionUnconfirmed,
            }
        })
    }
}

impl AppState {
    fn causal_test_admission_target(
        &self,
        delivery: &CausalTestCompletionDelivery,
    ) -> Result<EngramBindingTarget, CausalAdmissionCheck> {
        use CausalAdmissionCheck as Check;
        let scope = delivery
            .scope
            .as_ref()
            .ok_or(Check::SenderScopeUnavailable)?;
        let requester = &delivery.selector.payload.requester;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = federation_mailbox_root_locked(&inner, &requester.native_session_id)
            .map_err(|_| Check::RequesterRootUnavailable)?;
        if record.federation_native_creation_id != requester.native_creation_id {
            return Err(Check::RequesterCreationChanged);
        }
        if inner.federation.local.as_ref() != Some(&requester.host) {
            return Err(Check::RequesterHostChanged);
        }
        let target = Self::engram_source_root_target_locked(&inner, &requester.native_session_id)
            .map_err(|_| Check::DestinationTargetInvalid)?
            .ok_or(Check::DestinationTargetUnavailable)?;
        if target.project_id != scope.project_id {
            return Err(Check::DestinationProjectChanged);
        }
        if target.settings.authority_store_key.as_ref() != Some(&scope.store) {
            return Err(Check::DestinationStoreChanged);
        }
        if target.connection.session_id != requester.native_session_id {
            return Err(Check::DestinationSessionChanged);
        }
        Ok(target)
    }

    pub(crate) fn settle_causal_test_completion(
        &self,
        delivery: &CausalTestCompletionDelivery,
    ) -> Result<(), CausalSettlementFailure> {
        use CausalAdmissionCheck as Check;
        use CausalAdmissionFailure as Failure;
        let store = &self.mailbox_store;
        let selector = &delivery.selector;
        let scope = delivery.scope.as_ref();
        let original = store
            .causal_test_settlement(selector, scope)
            .map_err(CausalSettlementFailure::read)?;
        let mut saved = if let Some(original) = original {
            original
        } else {
            let original = CausalTestSettlement::new(delivery);
            // No connection or write has been attempted. Readback/COMMIT of the
            // exact original body must succeed before the first external write.
            store
                .save_causal_test_settlement(selector, scope, None, &original)
                .map_err(CausalSettlementFailure::save)?;
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
            return Err(CausalSettlementFailure {
                reason: CausalPendingReason::CompletionConflictsWithSettlement,
                error: anyhow!(
                    "conflicting original causal completion; retained operation is unchanged"
                ),
            });
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
            let target = self
                .causal_test_admission_target(delivery)
                .and_then(|target| {
                    if saved.admissions[index]
                        .connection
                        .as_ref()
                        .is_some_and(|original| original != &target.connection)
                    {
                        return Err(Check::DestinationConnectionChanged);
                    }
                    Ok(target)
                });
            let target = match target {
                Ok(target) => target,
                Err(check) => {
                    let code = check.stored();
                    saved.admissions[index].pending = Some(code.clone());
                    if before != saved {
                        store
                            .save_causal_test_settlement(selector, scope, Some(&before), &saved)
                            .map_err(CausalSettlementFailure::save)?;
                        eprintln!(
                            "engram> remote run={} target={} admission=pending ({code})",
                            saved.completion.run_id,
                            index + 1
                        );
                    }
                    continue;
                }
            };
            if saved.admissions[index].connection.is_none() {
                saved.admissions[index].connection = Some(target.connection.clone());
                store
                    .save_causal_test_settlement(selector, scope, Some(&before), &saved)
                    .map_err(CausalSettlementFailure::save)?;
            }
            let before = saved.clone();
            let admission = &mut saved.admissions[index];
            let request = admission
                .request
                .as_ref()
                .expect("validated original request");
            let reply = (|| {
                let scope = scope.expect("validated destination scope");
                target
                    .adapter
                    .read_work_session(
                        &target.connection,
                        &scope.store,
                        target.settings.call_timeout(),
                    )
                    .map_err(|error| Failure::from_transport(error, true))?;
                if !target
                    .adapter
                    .remote_verification_record_route_support(&target.connection)
                    .is_supported()
                {
                    return Err(Failure::Pending(Check::CapabilityUnavailable));
                }
                // This snapshot revalidation fences settings, actor, store and
                // runtime after capability I/O, without refreshing the captured
                // claim/root association. Engram judges NEW association currency.
                let now = self
                    .causal_test_admission_target(delivery)
                    .map_err(Failure::Pending)?;
                if now.connection != target.connection
                    || !now.settings.same_work_service_settings(&target.settings)
                    || now.runtime_snapshot != target.runtime_snapshot
                {
                    return Err(Failure::Pending(Check::DestinationSnapshotChanged));
                }
                target
                    .adapter
                    .request(
                        &target.connection,
                        &request.clone().into_control_request(),
                        target.settings.call_timeout(),
                    )
                    .map_err(|error| Failure::from_transport(error, false))
            })();
            match reply {
                Ok(value) => match request.receipt(value.clone()) {
                    Ok(_) => {
                        admission.receipt = Some(value);
                        admission.pending = None;
                    }
                    Err(_) => {
                        admission.pending = Some(Check::AdmissionReceiptInvalid.stored());
                    }
                },
                Err(Failure::Refused(code)) => {
                    admission.refusal = Some(code.into());
                    admission.pending = None;
                }
                Err(Failure::Pending(check)) => {
                    admission.pending = Some(check.stored());
                }
            }
            // Persist the actual ORIGINAL receipt before ending retry custody.
            // A crash before this COMMIT simply retries the same body/key.
            store
                .save_causal_test_settlement(selector, scope, Some(&before), &saved)
                .map_err(CausalSettlementFailure::save)?;
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
                    "engram> remote run={} target={} admission={decision} check={}",
                    saved.completion.run_id,
                    index + 1,
                    admission.pending.as_deref().unwrap_or("none")
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
pub(crate) struct CausalTestCompletionRecovery {
    pub(crate) task: Option<tokio::task::JoinHandle<()>>,
    pub(crate) observed_kick: u64,
    fallback_owed: bool,
    #[cfg(test)]
    pub(crate) stop_after_settlement: bool,
}

impl CausalTestCompletionRecovery {
    pub(crate) async fn tick(&mut self, app: &AppState, fallback: bool) {
        // A periodic tick remains owed while the joined worker is busy.
        self.fallback_owed |= fallback;
        if self.task.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        if let Some(task) = self.task.take() {
            if let Err(error) = task.await {
                app.record_causal_ready("worker_join_failed");
                eprintln!(
                    "causal completion recovery: worker_join_failed class={}; pending rows retained",
                    join_failure_class(&error)
                );
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
        #[cfg(test)]
        let stop_after_settlement = std::mem::take(&mut self.stop_after_settlement);
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
                            #[cfg(test)]
                            if stop_after_settlement {
                                // Simulate interruption after the settlement commit,
                                // before the worker publishes the outcome marker.
                                return result.map_err(|failure| mailbox_api_error(failure.error));
                            }
                            // The failed step names the row's reason; its error
                            // is logged here, before the row write can fail too.
                            let reason = match &result {
                                Ok(()) => CausalPendingReason::AdmissionPending,
                                Err(failure) => {
                                    eprintln!(
                                        "causal completion recovery: settlement deferred reason={:?} class={}; inspect durable pending diagnostics",
                                        failure.reason,
                                        settlement_error_class(&failure.error)
                                    );
                                    failure.reason
                                }
                            };
                            // A refused row write is this row's own outcome,
                            // counted apart from a worker that did not run.
                            if let Err(error) =
                                worker.mailbox_store.note_causal_pending_for_selector(
                                    &delivery.selector,
                                    delivery.pending.intents,
                                    reason,
                                )
                            {
                                worker.record_causal_ready("admission_pending_write_failed");
                                eprintln!(
                                    "causal completion recovery: admission_pending_write_failed reason={reason:?} class={}; original row retained",
                                    settlement_error_class(&error)
                                );
                            }
                            Ok(())
                        })
                        .await;
                        if let Err(error) = result {
                            app.record_causal_ready("admission_worker_failed");
                            eprintln!(
                                "causal completion recovery: admission_worker_failed status={}; original rows retained",
                                error.status.as_u16()
                            );
                        }
                    }
                }
                Err(error) => {
                    // A batch failure belongs to no single row. It is counted
                    // with the recovery decisions and logged with its cause.
                    app.record_causal_ready("pending_read_failed");
                    eprintln!(
                        "causal completion recovery: pending_read_failed status={}; original rows retained",
                        error.status.as_u16()
                    );
                }
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

    pub(crate) async fn shutdown(self) {
        if let Some(task) = self.task {
            if let Err(error) = task.await {
                eprintln!(
                    "causal completion recovery: worker_join_failed class={} at shutdown; pending rows retained",
                    join_failure_class(&error)
                );
            }
        }
    }
}
