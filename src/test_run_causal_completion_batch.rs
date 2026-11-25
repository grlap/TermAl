// Bounded work over explicit durable pending membership, never mailbox history.

use super::*;
const CAUSAL_COMPLETION_PENDING_ROWS: usize = 32;
pub(crate) const CAUSAL_COMPLETION_BATCH: usize = 4;

#[derive(Clone)]
pub(crate) struct CausalTestCompletionPlan {
    pub(crate) selector: CausalTestCompletionSelector,
    pub(crate) scope: Option<MailboxTestSenderScope>,
    pub(crate) peer: Option<FederationPeerSnapshot>,
    pub(crate) settlement: Option<CausalTestSettlement>,
    pub(crate) pending: CausalPendingRow,
}

impl MailboxStore {
    #[cfg(test)]
    pub(crate) fn causal_test_completion_plans(&self) -> Result<Vec<CausalTestCompletionPlan>> {
        self.causal_test_completion_plans_without(CAUSAL_COMPLETION_BATCH, &BTreeSet::new())
    }

    pub(crate) fn causal_test_completion_plans_without(
        &self,
        limit: usize,
        excluded: &BTreeSet<TestRunCausalKey>,
    ) -> Result<Vec<CausalTestCompletionPlan>> {
        let limit = limit.min(CAUSAL_COMPLETION_BATCH);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let local_store = mailbox_relay_store_id(&transaction)?;
        // Each branch uses the explicit-membership partial index. The merge and
        // original-row reads are bounded even when all historical mail is huge.
        let mut candidates = Vec::new();
        for intents in [false, true] {
            let table = CausalPendingRow { intents, row_id: 0 }.table();
            let mut statement = transaction.prepare(&format!(
                "SELECT rowid,test_request_pending_order FROM {table} WHERE test_request_pending_json IS NOT NULL ORDER BY test_request_pending_order LIMIT ?1"))?;
            let rows = statement.query_map([CAUSAL_COMPLETION_PENDING_ROWS as i64], |row| {
                Ok((
                    causal_pending_order(row, 1)?,
                    intents,
                    row.get::<_, i64>(0)?,
                ))
            })?;
            candidates.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        }
        candidates.sort_unstable();
        let mut plans = Vec::new();
        for (order, intents, row_id) in candidates.into_iter().take(CAUSAL_COMPLETION_PENDING_ROWS)
        {
            let pending = CausalPendingRow { intents, row_id };
            if order.is_none() {
                // Retry order is derived scheduling data. Repair only that
                // position, leaving all original bytes and metadata untouched;
                // this tick performs no origin read or admission for the key.
                rotate_causal_pending(&transaction, pending)?;
                continue;
            }
            let table = pending.table();
            let owner = if intents {
                "owner_session_id"
            } else {
                "sender_session_id"
            };
            let canonical = if intents { "receipt_json" } else { "id" };
            let peer = if intents { "peer_json" } else { "NULL" };
            let mut statement = transaction.prepare(&format!(
                "SELECT rowid,{owner},idempotency_key,test_request_json,test_request_scope_json,{canonical},{peer},test_request_settlement_json,test_request_pending_json FROM {table} WHERE rowid=?1"))?;
            let mut rows = statement.query([row_id])?;
            let row = rows
                .next()?
                .ok_or_else(|| anyhow!("pending original row disappeared"))?;
            match CausalPendingMetadata::from_column(row, 8)? {
                CausalPendingMetadata::Readable(_) => {}
                CausalPendingMetadata::Retired => continue,
                CausalPendingMetadata::Malformed => {
                    rotate_causal_pending(&transaction, pending)?;
                    continue;
                }
            }
            match causal_test_completion_plan_from_row(row, intents, &local_store) {
                Ok(CausalPendingRead::Ready(plan)) => {
                    // Hinted keys already own this batch's targeted slots.
                    // Do not rotate unattempted ordinary plans past them.
                    if excluded.contains(&plan.selector.key()) {
                        continue;
                    }
                    note_causal_pending(
                        &transaction,
                        pending,
                        CausalPendingReason::AwaitingCompletion,
                    )?;
                    plans.push(plan);
                }
                Ok(CausalPendingRead::Waiting(reason)) | Err(reason) => {
                    note_causal_pending(&transaction, pending, reason)?;
                }
                Ok(CausalPendingRead::NotOriginal) => {
                    note_causal_pending(
                        &transaction,
                        pending,
                        CausalPendingReason::OriginalCustodyChanged,
                    )?;
                }
                Ok(CausalPendingRead::Retired) => {
                    // A validated original final settlement is the only retirement.
                    transaction.execute(&format!("UPDATE {table} SET test_request_pending_json=NULL,test_request_pending_order=NULL WHERE rowid=?1"), [row_id])?;
                }
            }
            if plans.len() == limit {
                break;
            }
        }
        transaction.commit()?;
        Ok(plans)
    }
}

// A private handoff, never a wire or persisted projection.
pub(crate) struct CausalTestCompletionDelivery {
    pub(crate) selector: CausalTestCompletionSelector,
    pub(crate) scope: Option<MailboxTestSenderScope>,
    pub(crate) completion: native_test_completion::Completion,
    pub(crate) pending: CausalPendingRow,
}

impl AppState {
    async fn defer_causal_pending(
        &self,
        plan: &CausalTestCompletionPlan,
        reason: CausalPendingReason,
    ) -> Result<(), ApiError> {
        let store = self.mailbox_store.clone();
        let selector = plan.selector.clone();
        let intents = plan.pending.intents;
        run_blocking_api(move || {
            store
                .note_causal_pending_for_selector(&selector, intents, reason)
                .map_err(mailbox_api_error)
        })
        .await
    }
    pub(crate) fn causal_test_completion_plan_current(
        &self,
        plan: &CausalTestCompletionPlan,
    ) -> Result<(), ApiError> {
        let selector = &plan.selector;
        if self
            .native_mailbox_relay_descriptor(&selector.payload.requester.native_session_id)?
            .principal
            != selector.payload.requester
        {
            return Err(federation_stale_link());
        }
        let original = self
            .mailbox_store
            .retained_mailbox_test_payload(
                &selector.payload.requester.native_session_id,
                &selector.payload.request_send_key,
            )
            .map_err(mailbox_api_error)?;
        if original.as_ref() != Some(&(Some(selector.payload.clone()), plan.scope.clone())) {
            return Err(ApiError::conflict(
                "association_stale: original test request changed",
            ));
        }
        let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
        let mut statement = connection
            .prepare(
                "SELECT id,0 FROM mailbox_messages
            WHERE sender_session_id=?1 AND idempotency_key=?2
            UNION ALL SELECT receipt_json,1 FROM mailbox_relay_intents
            WHERE owner_session_id=?1 AND idempotency_key=?2 LIMIT 2",
            )
            .map_err(|error| mailbox_api_error(error.into()))?;
        let identities = statement
            .query_map(
                rusqlite::params![
                    selector.payload.requester.native_session_id,
                    selector.payload.request_send_key
                ],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, bool>(1)?)),
            )
            .map_err(|error| mailbox_api_error(error.into()))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| mailbox_api_error(error.into()))?;
        let canonical = match identities.as_slice() {
            [(Some(value), false)] => Some(value.clone()),
            [(Some(value), true)] => Some(
                serde_json::from_str::<MailboxAppendReceipt>(value)
                    .map_err(|_| ApiError::conflict("original test request receipt is malformed"))?
                    .message_id,
            ),
            _ => None,
        };
        if canonical.as_deref() != Some(selector.canonical_message_id.as_str()) {
            return Err(ApiError::conflict(
                "association_stale: original test request receipt changed",
            ));
        }
        drop(statement);
        drop(connection);
        if let Some(peer) = &plan.peer {
            let current = self.federation_peer_snapshot(&peer.peer.host_id)?;
            if !same_mailbox_relay_enrollment(peer, &current)
                || !mailbox_relay_peer_current_locked(
                    &self.inner.lock().expect("state mutex poisoned"),
                    &current,
                    peer.enrollment_generation,
                )
            {
                return Err(federation_stale_link());
            }
        }
        Ok(())
    }

    // The joined supervisor supplies the admission sink. Original frozen
    // pending operations recover without another origin read or live claim.
    #[cfg(test)]
    pub(crate) async fn read_causal_test_completion_batch(
        &self,
    ) -> Result<Vec<CausalTestCompletionDelivery>, ApiError> {
        let worker = self.clone();
        let plans = run_blocking_api(move || {
            worker
                .mailbox_store
                .causal_test_completion_plans()
                .map_err(mailbox_api_error)
        })
        .await?;
        self.read_causal_test_completion_plans(plans.into_iter().map(|plan| (plan, None)).collect())
            .await
    }

    // Every scheduler shares this original read/currentness/settlement handoff.
    // A hint can constrain run identity, never supply native facts.
    pub(crate) async fn read_causal_test_completion_plans(
        &self,
        plans: Vec<(CausalTestCompletionPlan, Option<String>)>,
    ) -> Result<Vec<CausalTestCompletionDelivery>, ApiError> {
        let mut deliveries = Vec::new();
        for (plan, expected_run) in plans {
            if self.federation_executor_wake_signal.is_stopping() {
                break;
            }
            if let Some(saved) = &plan.settlement {
                deliveries.push(CausalTestCompletionDelivery {
                    selector: plan.selector,
                    scope: plan.scope,
                    completion: saved.completion.clone(),
                    pending: plan.pending,
                });
                continue;
            }
            let worker = self.clone();
            let check = plan.clone();
            if run_blocking_api(move || worker.causal_test_completion_plan_current(&check))
                .await
                .is_err()
            {
                self.defer_causal_pending(&plan, CausalPendingReason::CurrentnessCheckFailed)
                    .await?;
                continue;
            }
            let runner = plan.selector.runner()?.clone();
            let Some(handle) = self.federation_peer_handle(&runner.host.host_id) else {
                self.defer_causal_pending(&plan, CausalPendingReason::RunnerUnavailable)
                    .await?;
                continue;
            };
            let Ok(lease) = handle.channel.current_lease() else {
                self.defer_causal_pending(&plan, CausalPendingReason::LeaseUnavailable)
                    .await?;
                continue;
            };
            if lease
                .with_current(self, |inner, peer| {
                    if peer.peer != runner.host
                        || peer.local != plan.selector.payload.requester.host
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
                .is_err()
            {
                self.defer_causal_pending(&plan, CausalPendingReason::RouteChanged)
                    .await?;
                continue;
            }
            let operation = MailboxRelayOperation::ReadCausalTestCompletion {
                selector: plan.selector.clone(),
            };
            let reply = match tokio::time::timeout(
                Duration::from_secs(5),
                self.request_mailbox_relay::<CausalTestCompletionReply>(&handle, &lease, operation),
            )
            .await
            {
                Ok(Ok(reply)) => reply,
                other => {
                    let reason = if other.is_err() {
                        CausalPendingReason::OriginTimeout
                    } else {
                        CausalPendingReason::OriginReadFailed
                    };
                    self.defer_causal_pending(&plan, reason).await?;
                    continue;
                }
            };
            let Some(completion) = reply.completion else {
                self.defer_causal_pending(&plan, CausalPendingReason::AwaitingCompletion)
                    .await?;
                continue;
            };
            let invalid = if expected_run
                .as_ref()
                .is_some_and(|run| *run != completion.run_id)
            {
                Some(CausalPendingReason::CompletionRunMismatch)
            } else if completion.validate().is_err() {
                Some(CausalPendingReason::CompletionValidate)
            } else if !completion.authoritative_terminal() {
                Some(CausalPendingReason::CompletionNotTerminal)
            } else if completion.origin.host_id != runner.host.host_id
                || completion.origin.runner_session_id != runner.native_session_id
            {
                Some(CausalPendingReason::CompletionOriginMismatch)
            } else if !matches!(completion.input, native_test_completion::Input::SourceId(_)) {
                Some(CausalPendingReason::CompletionInputUnsupported)
            } else if !matches!(
                (plan.selector.payload.mode, completion.mode),
                (MailboxTestMode::Full, native_test_completion::Mode::Full)
                    | (
                        MailboxTestMode::Focused,
                        native_test_completion::Mode::Focused
                    )
            ) {
                Some(CausalPendingReason::CompletionModeMismatch)
            } else {
                None
            };
            if let Some(reason) = invalid {
                self.defer_causal_pending(&plan, reason).await?;
                continue;
            }
            // Fresh route and original custody are checked again after I/O.
            if lease
                .with_current(self, |inner, peer| {
                    if peer.peer != runner.host
                        || peer.local != plan.selector.payload.requester.host
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
                .is_err()
            {
                self.defer_causal_pending(&plan, CausalPendingReason::PostReadRouteChanged)
                    .await?;
                continue;
            }
            let worker = self.clone();
            let check = plan.clone();
            if run_blocking_api(move || worker.causal_test_completion_plan_current(&check))
                .await
                .is_err()
            {
                self.defer_causal_pending(
                    &plan,
                    CausalPendingReason::PostReadCurrentnessCheckFailed,
                )
                .await?;
                continue;
            }
            deliveries.push(CausalTestCompletionDelivery {
                selector: plan.selector,
                scope: plan.scope,
                completion,
                pending: plan.pending,
            });
        }
        Ok(deliveries)
    }
}
