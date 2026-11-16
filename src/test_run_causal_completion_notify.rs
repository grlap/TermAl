// Runner-side completion-ready notifications. Disposable pending work is
// queued only when an original native completion first latches, never by
// traversing retained history. SQLite read-back still owns publication proof.
const CAUSAL_READY_NOTIFICATION_ATTEMPTS: u8 = 4;

struct CausalReadyNotification {
    hint: CausalTestCompletionReadyHint,
    attempts_left: u8,
}

impl AppState {
    // Called under the state lock at the original completion transition.
    // Queue loss or expiry is harmless: requester pending custody is durable.
    fn queue_causal_test_completion_notification(&self, registration: &TestRunRegistration) {
        let Some(causal) = test_run_causal_request(registration) else {
            return;
        };
        let hint = CausalTestCompletionReadyHint {
            requester: causal.payload.requester.clone(),
            runner: causal.runner.clone(),
            request_send_key: causal.request_send_key.clone(),
            canonical_message_id: causal.canonical_message_id.clone(),
            run_id: registration.run_id.clone(),
        };
        let key = hint.key();
        let mut queue = self
            .federation_executor_wake_signal
            .causal_ready
            .lock()
            .expect("ready mutex poisoned");
        if queue.notifications.len() < CAUSAL_READY_QUEUE_LIMIT {
            queue
                .notifications
                .entry(key)
                .or_insert_with(|| CausalReadyNotification {
                    hint,
                    attempts_left: CAUSAL_READY_NOTIFICATION_ATTEMPTS,
                });
        } else {
            queue.record("notification_queue_full_fallback_retained");
        }
    }

    async fn notify_causal_test_completions(&self) {
        let pending = {
            let mut queue = self
                .federation_executor_wake_signal
                .causal_ready
                .lock()
                .expect("ready mutex poisoned");
            (0..CAUSAL_COMPLETION_BATCH)
                .filter_map(|_| queue.notifications.pop_first())
                .collect::<Vec<_>>()
        };
        for (key, mut work) in pending {
            if self.federation_executor_wake_signal.is_stopping() {
                break;
            }
            work.attempts_left -= 1;
            match self
                .notify_original_causal_test_completion(&work.hint)
                .await
            {
                Ok(()) => self.record_causal_ready("notification_delivered"),
                Err(reason) => {
                    let mut queue = self
                        .federation_executor_wake_signal
                        .causal_ready
                        .lock()
                        .expect("ready mutex poisoned");
                    queue.record(reason);
                    if work.attempts_left > 0 {
                        // A new completion may have filled the bounded queue
                        // during I/O. Dropping this retry keeps fallback custody.
                        if queue.notifications.len() < CAUSAL_READY_QUEUE_LIMIT {
                            queue.notifications.entry(key).or_insert(work);
                        }
                    } else {
                        queue.record("notification_attempts_exhausted_fallback_retained");
                    }
                }
            }
        }
    }

    async fn notify_original_causal_test_completion(
        &self,
        hint: &CausalTestCompletionReadyHint,
    ) -> Result<(), &'static str> {
        hint.validate()
            .map_err(|_| "notification_original_identity_changed")?;
        // Compaction may replace the registration image without changing its
        // original causal identity. Resolve that current image, then require
        // exact durable read-back and route checks against the same image.
        let worker = self.clone();
        let run_id = hint.run_id.clone();
        let expected = run_blocking_api(move || {
            let inner = worker.inner.lock().expect("state mutex poisoned");
            Ok(test_run_retained_registration(&inner, &run_id).cloned())
        })
        .await
        .map_err(|_| "notification_original_identity_changed")?
        .ok_or("notification_original_identity_changed")?;
        let causal =
            test_run_causal_request(&expected).ok_or("notification_original_identity_changed")?;
        let selector = CausalTestCompletionSelector {
            canonical_message_id: causal.canonical_message_id.clone(),
            payload: causal.payload.clone(),
        };
        let runner = causal.runner.clone();
        let requester = causal.payload.requester.clone();
        if selector.validate().is_err()
            || expected.run_id != hint.run_id
            || selector.key() != hint.key()
            || runner != hint.runner
            || requester != hint.requester
            || !selector.matches(&expected)
            || expected.native_origin.as_ref().is_none_or(|origin| {
                origin.host != runner.host
                    || origin.native_session_id != runner.native_session_id
                    || origin.native_creation_id != runner.native_creation_id
                    || expected.owning_session_id != runner.native_session_id
                    || expected.initiating_session_id != runner.native_session_id
                    || expected.project_id.as_deref() != Some(origin.project_id.as_str())
            })
        {
            return Err("notification_original_identity_changed");
        }
        let worker = self.clone();
        let principal = runner.clone();
        let descriptor = run_blocking_api(move || {
            worker
                .native_mailbox_relay_descriptor(&principal.native_session_id)
                .map(|own| own.principal == principal)
        })
        .await;
        if !matches!(descriptor, Ok(true)) {
            return Err("notification_original_identity_changed");
        }
        let handle = self
            .federation_peer_handle(&requester.host.host_id)
            .ok_or("notification_requester_unavailable")?;
        let lease = handle
            .channel
            .current_lease()
            .map_err(|_| "notification_lease_unavailable")?;
        let current = || {
            lease.with_current(self, |inner, peer| {
                if peer.local != runner.host
                    || peer.peer != requester.host
                    || !mailbox_relay_peer_current_locked(inner, peer, peer.enrollment_generation)
                    || test_run_retained_registration(inner, &expected.run_id) != Some(&expected)
                {
                    return Err(federation_stale_link());
                }
                Ok(())
            })
        };
        current().map_err(|_| "notification_route_or_registration_changed")?;
        let worker = self.clone();
        let registration = expected.clone();
        let durable = run_blocking_api(move || {
            worker.read_durable_native_test_run_for_notification(&registration)
        })
        .await;
        let Ok(Some(DurableNativeTestRun {
            completion: Some(completion),
        })) = durable
        else {
            return Err("notification_completion_not_durable");
        };
        if completion.validate().is_err()
            || !completion.authoritative_terminal()
            || completion.run_id != expected.run_id
            || completion.origin.host_id != runner.host.host_id
            || completion.origin.runner_session_id != runner.native_session_id
            || !matches!(completion.input, native_test_completion::Input::SourceId(_))
            || !matches!(
                (selector.payload.mode, completion.mode),
                (MailboxTestMode::Full, native_test_completion::Mode::Full)
                    | (
                        MailboxTestMode::Focused,
                        native_test_completion::Mode::Focused
                    )
            )
            || current().is_err()
        {
            return Err("notification_completion_or_currentness_refused");
        }
        let reply = tokio::time::timeout(
            Duration::from_secs(5),
            self.request_mailbox_relay::<Value>(
                &handle,
                &lease,
                MailboxRelayOperation::CausalTestCompletionReady { hint: hint.clone() },
            ),
        )
        .await;
        if matches!(reply, Ok(Ok(ref value)) if value["status"] == "scheduled"
            || value["status"] == "ignored" && matches!(value["reason"].as_str(),
                Some("original_already_scheduled" | "original_already_settled")))
        {
            Ok(())
        } else {
            Err("notification_unconfirmed_retry_original")
        }
    }
}
