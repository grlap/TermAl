// Completion-ready hints over the existing authenticated mailbox relay.
// Owns bounded disposable scheduling and diagnostics, not results, receipts,
// original pending membership or admission. New owner, not a source split.

use super::*;
pub(crate) const CAUSAL_READY_QUEUE_LIMIT: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CausalTestCompletionReadyHint {
    pub(crate) requester: MailboxRelayPrincipal,
    pub(crate) runner: MailboxRelayPrincipal,
    pub(crate) request_send_key: String,
    pub(crate) canonical_message_id: String,
    pub(crate) run_id: String,
}

impl CausalTestCompletionReadyHint {
    pub(crate) fn key(&self) -> TestRunCausalKey {
        (
            self.requester.participant_key(),
            self.request_send_key.clone(),
            self.canonical_message_id.clone(),
        )
    }

    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        self.requester.validate()?;
        self.runner.validate()?;
        if self.request_send_key.is_empty()
            || self.request_send_key.len() > 256
            || !self
                .canonical_message_id
                .strip_prefix("mailbox-message-")
                .is_some_and(mailbox_relay_uuid)
            || self.run_id.is_empty()
            || self.run_id.len() > 256
            || !self
                .run_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(ApiError::bad_request("completion_ready_identity_invalid"));
        }
        Ok(())
    }

    pub(crate) fn matches(&self, plan: &CausalTestCompletionPlan) -> bool {
        plan.selector.key() == self.key()
            && plan.selector.payload.requester == self.requester
            && plan
                .selector
                .runner()
                .is_ok_and(|runner| runner == &self.runner)
    }
}

#[derive(Default)]
pub(crate) struct CausalCompletionReadyQueue {
    pub(crate) pending: BTreeMap<TestRunCausalKey, CausalTestCompletionReadyHint>,
    pub(crate) decisions: BTreeMap<&'static str, u64>,
    pub(crate) notifications: BTreeMap<TestRunCausalKey, CausalReadyNotification>,
}

impl CausalCompletionReadyQueue {
    pub(crate) fn record(&mut self, reason: &'static str) {
        let count = self.decisions.entry(reason).or_default();
        *count = count.saturating_add(1);
        eprintln!("causal completion ready: {reason}");
    }

    pub(crate) fn diagnostic(&self) -> Value {
        json!({"queued":self.pending.len(),"notifications":self.notifications.len(),"decisions":self.decisions,"resetOnRestart":true})
    }
}

impl AppState {
    pub(crate) fn record_causal_ready(&self, reason: &'static str) {
        self.federation_executor_wake_signal
            .causal_ready
            .lock()
            .expect("ready mutex poisoned")
            .record(reason);
    }

    pub(crate) fn receive_causal_test_completion_ready(
        &self,
        hint: CausalTestCompletionReadyHint,
        lease: &FederationLinkLease,
        authenticated: &FederationHostIdentity,
    ) -> Result<Value, ApiError> {
        let result = (|| {
            hint.validate()?;
            hint.runner.require_origin(authenticated)?;
            let current = || {
                lease.with_current(self, |inner, peer| {
                    if peer.peer != hint.runner.host
                        || peer.local != hint.requester.host
                        || !mailbox_relay_peer_current_locked(
                            inner,
                            peer,
                            peer.enrollment_generation,
                        )
                    {
                        return Err(federation_stale_link());
                    }
                    Ok(())
                })
            };
            current()?;
            if self
                .native_mailbox_relay_descriptor(&hint.requester.native_session_id)?
                .principal
                != hint.requester
            {
                return Err(ApiError::conflict("completion_ready_requester_changed"));
            }
            let plan = match self
                .mailbox_store
                .causal_test_completion_plan_for_key(&hint.requester, &hint.key())
                .map_err(mailbox_api_error)?
            {
                CausalReadyPlan::Ready(plan) => plan,
                CausalReadyPlan::Ignored(reason) => {
                    self.record_causal_ready(reason);
                    return Ok(json!({"status":"ignored","reason":reason}));
                }
            };
            if !hint.matches(&plan) {
                return Err(ApiError::conflict(
                    "completion_ready_original_identity_changed",
                ));
            }
            self.causal_test_completion_plan_current(&plan)?;
            current()?;
            let mut queue = self
                .federation_executor_wake_signal
                .causal_ready
                .lock()
                .expect("ready mutex poisoned");
            let key = hint.key();
            if queue.pending.contains_key(&key) {
                queue.record("original_already_scheduled");
                return Ok(json!({"status":"ignored","reason":"original_already_scheduled"}));
            }
            if queue.pending.len() == CAUSAL_READY_QUEUE_LIMIT {
                queue.record("ready_queue_full_fallback_retained");
                return Ok(
                    json!({"status":"ignored","reason":"ready_queue_full_fallback_retained"}),
                );
            }
            queue.pending.insert(key, hint);
            queue.record("scheduled_original_key");
            drop(queue);
            self.federation_executor_wake_signal.kick();
            Ok(json!({"status":"scheduled"}))
        })();
        if result.is_err() {
            self.record_causal_ready("ready_authentication_or_currentness_refused");
        }
        result
    }

    async fn take_causal_ready_plans(
        &self,
        fallback: bool,
    ) -> Result<Vec<(CausalTestCompletionPlan, Option<String>)>, ApiError> {
        let limit = if fallback {
            CAUSAL_COMPLETION_BATCH / 2
        } else {
            CAUSAL_COMPLETION_BATCH
        };
        let hints = {
            let mut queue = self
                .federation_executor_wake_signal
                .causal_ready
                .lock()
                .expect("ready mutex poisoned");
            (0..limit)
                .filter_map(|_| queue.pending.pop_first().map(|(_, hint)| hint))
                .collect::<Vec<_>>()
        };
        let mut plans = Vec::new();
        for hint in hints {
            let store = self.mailbox_store.clone();
            let key = hint.key();
            let requester = hint.requester.clone();
            match run_blocking_api(move || {
                store
                    .causal_test_completion_plan_for_key(&requester, &key)
                    .map_err(mailbox_api_error)
            })
            .await?
            {
                CausalReadyPlan::Ready(plan) if hint.matches(&plan) => {
                    plans.push((plan, Some(hint.run_id)))
                }
                CausalReadyPlan::Ignored(reason) => self.record_causal_ready(reason),
                _ => self.record_causal_ready("queued_original_identity_changed"),
            }
        }
        Ok(plans)
    }

    pub(crate) async fn read_ready_causal_test_completions(
        &self,
        fallback: bool,
    ) -> Result<Vec<CausalTestCompletionDelivery>, ApiError> {
        let mut plans = self.take_causal_ready_plans(fallback).await?;
        // Periodic fallback always retains room for the original indexed sweep;
        // a stream of hints cannot starve lost-notification/restart recovery.
        if fallback || plans.is_empty() {
            let worker = self.clone();
            let available = CAUSAL_COMPLETION_BATCH - plans.len();
            let targeted = plans.iter().map(|(plan, _)| plan.selector.key()).collect();
            let ordinary = run_blocking_api(move || {
                worker
                    .mailbox_store
                    .causal_test_completion_plans_without(available, &targeted)
                    .map_err(mailbox_api_error)
            })
            .await?;
            for plan in ordinary {
                if plans.len() == CAUSAL_COMPLETION_BATCH {
                    break;
                }
                if !plans
                    .iter()
                    .any(|(queued, _)| queued.selector.key() == plan.selector.key())
                {
                    plans.push((plan, None));
                }
            }
        }
        self.read_causal_test_completion_plans(plans).await
    }
}
