// Derived work only. Original requests, native facts and Engram receipts keep
// their existing durable owners; this cursor is disposable scheduling state.
const CAUSAL_COMPLETION_SCAN_ROWS: usize = 32;
const CAUSAL_COMPLETION_BATCH: usize = 4;

#[allow(dead_code)] // Slice 3 supplies the supervisor caller and admission sink.
#[derive(Clone, Default)]
struct CausalTestCompletionCursor {
    messages_after: i64,
    intents_after: i64,
    intents: bool,
}

struct CausalTestCompletionPlan {
    selector: CausalTestCompletionSelector,
    scope: Option<MailboxTestSenderScope>,
    peer: Option<FederationPeerSnapshot>,
}

#[allow(dead_code)] // A private handoff, never a wire or persisted projection.
struct CausalTestCompletionDelivery {
    selector: CausalTestCompletionSelector,
    scope: Option<MailboxTestSenderScope>,
    completion: native_test_completion::Completion,
}

impl MailboxStore {
    fn causal_test_completion_plans(
        &self,
        cursor: &mut CausalTestCompletionCursor,
    ) -> Result<Vec<CausalTestCompletionPlan>> {
        let connection = self.connection()?;
        let local_store = mailbox_relay_store_id(&connection)?;
        // LIMIT applies BEFORE custody filtering: even a long ordinary-mail
        // history costs at most this many rows. No unindexed IS NOT NULL scan.
        let (query, after) = if cursor.intents {
            ("SELECT rowid,owner_session_id,idempotency_key,test_request_json,test_request_scope_json,receipt_json,peer_json
              FROM mailbox_relay_intents WHERE rowid>?1 ORDER BY rowid LIMIT ?2", cursor.intents_after)
        } else {
            ("SELECT rowid,sender_session_id,idempotency_key,test_request_json,test_request_scope_json,id,NULL
              FROM mailbox_messages WHERE rowid>?1 ORDER BY rowid LIMIT ?2", cursor.messages_after)
        };
        let mut statement = connection.prepare(query)?;
        let mut rows =
            statement.query(rusqlite::params![after, CAUSAL_COMPLETION_SCAN_ROWS as i64])?;
        let mut plans = Vec::new();
        let mut visited = 0;
        let mut last = after;
        while let Some(row) = rows.next()? {
            visited += 1;
            last = row.get(0)?;
            let Some(payload) = mailbox_test_payload_from_row(row, 3)? else {
                continue;
            };
            let owner: String = row.get(1)?;
            let key: String = row.get(2)?;
            // A relay home also has a public copy, with a participant-key
            // sender. Only the original local owner or sender intent schedules.
            if owner != payload.requester.native_session_id
                || key != payload.request_send_key
                || payload.requester.store_id != local_store
            {
                continue;
            }
            let stored: Option<String> = row.get(5)?;
            let canonical_message_id = if cursor.intents {
                // An unsettled original send stays pending in its existing
                // owner. This reader never guesses its home-assigned id.
                let Some(receipt) = stored else {
                    continue;
                };
                serde_json::from_str::<MailboxAppendReceipt>(&receipt)?.message_id
            } else {
                let Some(id) = stored else {
                    continue;
                };
                id
            };
            let selector = CausalTestCompletionSelector {
                canonical_message_id,
                payload,
            };
            selector
                .validate()
                .map_err(|error| relay_store_error(&error.message))?;
            let scope = mailbox_test_scope_from_row(row, 4)?;
            let peer: Option<String> = row.get(6)?;
            let peer = peer.map(|json| serde_json::from_str(&json)).transpose()?;
            plans.push(CausalTestCompletionPlan {
                selector,
                scope,
                peer,
            });
            if plans.len() == CAUSAL_COMPLETION_BATCH {
                break;
            }
        }
        // A short page may have stopped on the result limit, rather than EOF.
        let next = if visited < CAUSAL_COMPLETION_SCAN_ROWS && plans.len() < CAUSAL_COMPLETION_BATCH
        {
            0
        } else {
            last
        };
        if cursor.intents {
            cursor.intents_after = next;
        } else {
            cursor.messages_after = next;
        }
        cursor.intents = !cursor.intents;
        Ok(plans)
    }
}

impl AppState {
    fn causal_test_completion_plan_current(
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

    // Slice 3 owns calling this from the joined federation supervisor, then
    // building/admitting Engram requests and checking its receipts. Until that
    // slice lands this has no production caller, by the staged-slice contract.
    // No no-op consumer, new fact cache or persistent delivered bit exists.
    #[allow(dead_code)]
    async fn read_causal_test_completion_batch(
        &self,
        cursor: &mut CausalTestCompletionCursor,
    ) -> Result<Vec<CausalTestCompletionDelivery>, ApiError> {
        let worker = self.clone();
        let mut next = cursor.clone();
        let (next, plans) = run_blocking_api(move || {
            let plans = worker
                .mailbox_store
                .causal_test_completion_plans(&mut next)
                .map_err(mailbox_api_error)?;
            Ok((next, plans))
        })
        .await?;
        *cursor = next;
        let mut deliveries = Vec::new();
        for plan in plans {
            if self.federation_executor_wake_signal.is_stopping() {
                break;
            }
            let worker = self.clone();
            let check = CausalTestCompletionPlan {
                selector: plan.selector.clone(),
                scope: plan.scope.clone(),
                peer: plan.peer.clone(),
            };
            if run_blocking_api(move || worker.causal_test_completion_plan_current(&check))
                .await
                .is_err()
            {
                continue;
            }
            let runner = plan.selector.runner()?.clone();
            let Some(handle) = self.federation_peer_handle(&runner.host.host_id) else {
                continue;
            };
            let Ok(lease) = handle.channel.current_lease() else {
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
                continue;
            }
            let operation = MailboxRelayOperation::ReadCausalTestCompletion {
                selector: plan.selector.clone(),
            };
            let Ok(Ok(reply)) = tokio::time::timeout(
                Duration::from_secs(5),
                self.request_mailbox_relay::<CausalTestCompletionReply>(&handle, &lease, operation),
            )
            .await
            else {
                continue;
            };
            let Some(completion) = reply.completion else {
                continue;
            };
            if completion.validate().is_err()
                || !completion.authoritative_terminal()
                || completion.origin.host_id != runner.host.host_id
                || completion.origin.runner_session_id != runner.native_session_id
                || !matches!(completion.input, native_test_completion::Input::SourceId(_))
                || !matches!(
                    (plan.selector.payload.mode, completion.mode),
                    (MailboxTestMode::Full, native_test_completion::Mode::Full)
                        | (
                            MailboxTestMode::Focused,
                            native_test_completion::Mode::Focused
                        )
                )
            {
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
                continue;
            }
            let worker = self.clone();
            let check = CausalTestCompletionPlan {
                selector: plan.selector.clone(),
                scope: plan.scope.clone(),
                peer: plan.peer.clone(),
            };
            if run_blocking_api(move || worker.causal_test_completion_plan_current(&check))
                .await
                .is_err()
            {
                continue;
            }
            deliveries.push(CausalTestCompletionDelivery {
                selector: plan.selector,
                scope: plan.scope,
                completion,
            });
        }
        Ok(deliveries)
    }
}
