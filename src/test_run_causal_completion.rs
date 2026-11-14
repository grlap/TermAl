// Original-request completion reads over the authenticated mailbox relay.
// This owner never imports observer facts, probes artifacts, or grants credit.

type TestRunCausalKey = (String, String, String);

fn test_run_causal_key(registration: &TestRunRegistration) -> Option<TestRunCausalKey> {
    let causal = test_run_causal_request(registration)?;
    Some((
        causal.payload.requester.participant_key(),
        causal.request_send_key.clone(),
        causal.canonical_message_id.clone(),
    ))
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CausalTestCompletionSelector {
    canonical_message_id: String,
    payload: MailboxTestPayload,
}

impl CausalTestCompletionSelector {
    fn key(&self) -> TestRunCausalKey {
        (
            self.payload.requester.participant_key(),
            self.payload.request_send_key.clone(),
            self.canonical_message_id.clone(),
        )
    }

    fn runner(&self) -> Result<&MailboxRelayPrincipal, ApiError> {
        self.payload.recipient.as_ref().ok_or_else(|| {
            ApiError::bad_request("invalid_request: causal completion recipient is missing")
        })
    }

    fn validate(&self) -> Result<(), ApiError> {
        validate_mailbox_test_payload(&self.payload)?;
        self.runner()?.validate()?;
        if !self
            .canonical_message_id
            .strip_prefix("mailbox-message-")
            .is_some_and(mailbox_relay_uuid)
        {
            return Err(ApiError::bad_request(
                "invalid_request: canonical request message is required",
            ));
        }
        Ok(())
    }

    fn matches(&self, row: &TestRunRegistration) -> bool {
        test_run_causal_request(row).is_some_and(|causal| {
            causal.canonical_message_id == self.canonical_message_id
                && causal.request_send_key == self.payload.request_send_key
                && causal.payload == self.payload
                && Some(&causal.runner) == self.payload.recipient.as_ref()
        })
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CausalTestCompletionReply {
    // No local destination association or private sender store is returned.
    completion: Option<native_test_completion::Completion>,
}

struct DurableNativeTestRun {
    completion: Option<native_test_completion::Completion>,
}

impl AppState {
    // Shared by causal and observer projections. One SQLite snapshot proves
    // both the exact registration and its original provider execution, even
    // after compaction. An in-memory terminal alone cannot be published.
    fn read_durable_native_test_run(
        &self,
        expected: &TestRunRegistration,
    ) -> Result<Option<DurableNativeTestRun>, ApiError> {
        let run = expected.run_id.as_str();
        let mut connection = open_sqlite_state_read_connection(self.persistence_path.as_path())
            .map_err(|_| ApiError::internal("native completion persistence is unavailable"))?;
        let transaction = connection
            .transaction()
            .map_err(|_| ApiError::internal("native completion snapshot is unavailable"))?;
        let metadata = read_metadata_for_fence(
            &transaction,
            &[
                "testRunRegistrations",
                "testRunCompacted",
                "testRunObservations",
            ],
        )
        .map_err(|_| ApiError::internal("native completion read-back failed"))?;
        let Some(metadata) = metadata else {
            return Ok(None);
        };
        let compacted = metadata
            .test_run_compacted
            .get(run)
            .filter(|row| row.compacted && row.summary.run_id == run);
        let stored = metadata
            .test_run_registrations
            .get(run)
            .or_else(|| compacted.and_then(|row| row.registration.as_ref()));
        if stored != Some(expected) {
            return Ok(None);
        }
        let completion = expected
            .native_completion
            .as_ref()
            .filter(|retained| {
                metadata
                    .test_run_observations
                    .get(&retained.execution_id)
                    .or_else(|| {
                        compacted
                            .filter(|row| row.registration.as_ref() == Some(expected))
                            .and_then(|row| {
                                let mut exact = row
                                    .observations
                                    .iter()
                                    .filter(|row| row.execution_id == retained.execution_id);
                                exact.next().filter(|_| exact.next().is_none())
                            })
                    })
                    .is_some_and(|row| {
                        row.runtime_identity.is_some()
                            && row.summary.ended_at.is_some()
                            && row.summary.owner_session_id.as_deref()
                                == Some(expected.owning_session_id.as_str())
                            && row.artifact.as_ref().is_some_and(|artifact| {
                                artifact.run_id == run
                                    && artifact.request_identity.as_deref()
                                        == Some(expected.request_identity.as_str())
                            })
                    })
            })
            .map(|retained| retained.completion.clone());
        Ok(Some(DurableNativeTestRun { completion }))
    }

    fn read_causal_test_completion(
        &self,
        selector: &CausalTestCompletionSelector,
        lease: &FederationLinkLease,
        authenticated: &FederationHostIdentity,
    ) -> Result<CausalTestCompletionReply, ApiError> {
        selector.validate()?;
        selector.payload.requester.require_origin(authenticated)?;
        let runner = selector.runner()?;
        let store = {
            let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
            mailbox_relay_store_id(&connection).map_err(mailbox_api_error)?
        };
        let expected = lease.with_current(self, |inner, peer| {
            if peer.local != runner.host
                || runner.store_id != store
                || peer.peer != selector.payload.requester.host
                || !mailbox_relay_peer_current_locked(inner, peer, peer.enrollment_generation)
            {
                return Err(federation_stale_link());
            }
            let key = selector.key();
            // These are maintained indexes of the existing registration and
            // compaction owners, rebuilt on reload, not a second run registry.
            let mut rows = inner
                .test_run_registrations
                .index()
                .causal(&key)
                .filter_map(|key| inner.test_run_registrations.get(key))
                .chain(
                    inner
                        .test_run_compacted
                        .index()
                        .causal(&key)
                        .filter_map(|key| inner.test_run_compacted.get(key))
                        .filter(|row| row.compacted)
                        .filter_map(|row| row.registration.as_ref()),
                );
            let Some(row) = rows.next() else {
                return Ok(None);
            };
            if rows.next().is_some() || !selector.matches(row) {
                return Err(ApiError::conflict(
                    "association_stale: original causal registration is ambiguous or changed",
                ));
            }
            let Some(origin) = row.native_origin.as_ref() else {
                return Ok(None);
            };
            if origin.host != runner.host
                || origin.native_session_id != runner.native_session_id
                || origin.native_creation_id != runner.native_creation_id
                || row.owning_session_id != runner.native_session_id
                || row.initiating_session_id != runner.native_session_id
                || row.project_id.as_deref() != Some(origin.project_id.as_str())
            {
                return Err(ApiError::conflict(
                    "association_stale: causal native origin changed",
                ));
            }
            Ok(Some(row.clone()))
        })?;
        let Some(expected) = expected else {
            return Ok(CausalTestCompletionReply { completion: None });
        };
        let durable = self.read_durable_native_test_run(&expected)?;
        let completion = durable.and_then(|row| row.completion);
        if let Some(completion) = &completion {
            completion.validate()?;
            if completion.origin.host_id != runner.host.host_id
                || completion.origin.runner_session_id != runner.native_session_id
                || completion.run_id != expected.run_id
                || !matches!(
                    (selector.payload.mode, completion.mode),
                    (MailboxTestMode::Full, native_test_completion::Mode::Full)
                        | (
                            MailboxTestMode::Focused,
                            native_test_completion::Mode::Focused
                        )
                )
                || !completion.authoritative_terminal()
                || !matches!(completion.input, native_test_completion::Input::SourceId(_))
            {
                return Err(ApiError::conflict(
                    "producer_unsupported: causal native completion does not match the original request",
                ));
            }
        }
        let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
        if mailbox_relay_store_id(&connection).map_err(mailbox_api_error)? != runner.store_id {
            return Err(federation_stale_link());
        }
        drop(connection);
        lease.with_current(self, |inner, peer| {
            if peer.local != runner.host
                || peer.peer != selector.payload.requester.host
                || !mailbox_relay_peer_current_locked(inner, peer, peer.enrollment_generation)
                || test_run_retained_registration(inner, &expected.run_id) != Some(&expected)
            {
                return Err(federation_stale_link());
            }
            Ok(())
        })?;
        Ok(CausalTestCompletionReply { completion })
    }
}
