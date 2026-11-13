// Resolves an explicit message selection through the existing mailbox reader.
// The registration owner publishes custody and its ACK in its existing write.

enum TestRunCausalAdmission {
    Direct,
    Api {
        selector: Option<String>,
        message: Option<TestRunSelectedMessage>,
    },
}

struct TestRunSelectedMessage {
    selector: String,
    canonical: String,
    message: MailboxMessage,
    runner: MailboxRelayPrincipal,
    project_id: String,
    peer: Option<FederationPeerSnapshot>,
}

struct TestRunCausalCapture {
    causal: TestRunCausalRequest,
    project_id: String,
    peer: Option<FederationPeerSnapshot>,
}

fn test_run_request_message_id(request: &Value) -> Result<Option<&str>, ApiError> {
    let Some(value) = request.get("requestMessageId") else {
        return Ok(None);
    };
    value
        .as_str()
        .filter(|value| mailbox_test_message_selector_valid(value))
        .map(Some)
        .ok_or_else(|| ApiError::bad_request("invalid_request: malformed requestMessageId"))
}

fn test_run_causal_request(registration: &TestRunRegistration) -> Option<&TestRunCausalRequest> {
    registration.native_origin.as_ref()?.causal_request.as_ref()
}

fn test_run_retained_registration<'a>(
    inner: &'a StateInner,
    run: &str,
) -> Option<&'a TestRunRegistration> {
    inner
        .test_run_registrations
        .get(run)
        .or_else(|| inner.test_run_compacted.get(run)?.registration.as_ref())
}

impl TestRunCausalCapture {
    fn validate_locked(
        &self,
        inner: &StateInner,
        session: &str,
        run: &str,
    ) -> Result<(), ApiError> {
        let record = federation_mailbox_root_locked(inner, session)?;
        let runner = &self.causal.runner;
        if runner.native_session_id != session
            || record.federation_native_creation_id != runner.native_creation_id
            || inner.federation.local.as_ref() != Some(&runner.host)
            || record.session.project_id.as_deref() != Some(&self.project_id)
        {
            return Err(ApiError::conflict(
                "selected test-request runner identity or project changed",
            ));
        }
        if let Some(peer) = &self.peer {
            let now = federation_snapshot_locked(inner, &peer.peer.host_id)?;
            if !same_mailbox_relay_enrollment(peer, &now) {
                return Err(ApiError::conflict(
                    "selected test-request authenticated peer changed",
                ));
            }
        }
        // Both maps are durable survivors of this same registration owner.
        // This check and insertion share StateInner; concurrent new runs cannot
        // consume one request twice. No separate reservation ledger is needed.
        for earlier in inner.test_run_registrations.values().chain(
            inner
                .test_run_compacted
                .values()
                .filter_map(|row| row.registration.as_ref()),
        ) {
            if earlier.run_id == run {
                continue;
            }
            if test_run_causal_request(earlier).is_some_and(|old| {
                old.payload.requester == self.causal.payload.requester
                    && old.request_send_key == self.causal.request_send_key
            }) {
                return Err(ApiError::conflict(
                    "selected test request is already consumed by another run; replacement needs a new message",
                ));
            }
        }
        Ok(())
    }
}

impl AppState {
    fn selected_test_project(&self, session: &str) -> Result<String, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        federation_mailbox_root_locked(&inner, session)?
            .session
            .project_id
            .clone()
            .ok_or_else(|| ApiError::conflict("selected test-request runner has no local project"))
    }

    fn selected_local_test_message(
        &self,
        session: &str,
        selector: &str,
    ) -> Result<TestRunSelectedMessage, ApiError> {
        if MailboxRelayLocator::decode(selector, true)?.is_some() {
            return Err(ApiError::conflict(
                "selected remote message requires the authenticated registration route",
            ));
        }
        let runner = self.native_mailbox_relay_descriptor(session)?.principal;
        let message = self
            .mailbox_store
            .read_message(session, selector)
            .map_err(mailbox_api_error)?;
        Ok(TestRunSelectedMessage {
            selector: selector.to_owned(),
            canonical: selector.to_owned(),
            message,
            runner,
            project_id: self.selected_test_project(session)?,
            peer: None,
        })
    }

    async fn prepare_test_run_causal_admission(
        &self,
        session: &str,
        run: &str,
        selector: Option<String>,
    ) -> Result<TestRunCausalAdmission, ApiError> {
        let Some(value) = selector.as_deref() else {
            return Ok(TestRunCausalAdmission::Api {
                selector,
                message: None,
            });
        };
        if !mailbox_test_message_selector_valid(value) {
            return Err(ApiError::bad_request(
                "invalid_request: malformed requestMessageId",
            ));
        }
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(earlier) = test_run_retained_registration(&inner, run) {
                if test_run_causal_request(earlier)
                    .is_none_or(|old| old.request_message_id != value)
                    || earlier.initiating_session_id != session
                {
                    return Err(ApiError::conflict(
                        "an existing run cannot adopt or change its original test request",
                    ));
                }
                return Ok(TestRunCausalAdmission::Api {
                    selector,
                    message: None,
                });
            }
        }
        let message = if let Some(locator) = MailboxRelayLocator::decode(value, true)? {
            let runner = self.native_mailbox_relay_descriptor(session)?.principal;
            let project_id = self.selected_test_project(session)?;
            let peer = if locator.home.host != runner.host {
                let inner = self.inner.lock().expect("state mutex poisoned");
                Some(federation_snapshot_locked(
                    &inner,
                    &locator.home.host.host_id,
                )?)
            } else {
                None
            };
            let canonical = locator.id.clone();
            let message = self
                .read_native_mailbox_relay_message(session, locator)
                .await?;
            TestRunSelectedMessage {
                selector: value.to_owned(),
                canonical,
                message,
                runner,
                project_id,
                peer,
            }
        } else {
            let app = self.clone();
            let session = session.to_owned();
            let selector = value.to_owned();
            run_blocking_api(move || app.selected_local_test_message(&session, &selector)).await?
        };
        Ok(TestRunCausalAdmission::Api {
            selector,
            message: Some(message),
        })
    }

    fn resolve_test_run_causal_admission(
        &self,
        session: &str,
        run: &str,
        request: &Value,
        identity: &str,
        admission: TestRunCausalAdmission,
    ) -> Result<Option<TestRunCausalCapture>, ApiError> {
        let selected = test_run_request_message_id(request)?;
        let prepared = match admission {
            TestRunCausalAdmission::Direct => None,
            TestRunCausalAdmission::Api { selector, message } => {
                if selector.as_deref() != selected {
                    return Err(ApiError::conflict(
                        "registration selector differs from its immutable launcher request",
                    ));
                }
                message
            }
        };
        let Some(selector) = selected else {
            return Ok(None);
        };
        let current_runner = self.native_mailbox_relay_descriptor(session)?.principal;
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(earlier) = test_run_retained_registration(&inner, run) {
                if earlier.request_identity != identity || earlier.initiating_session_id != session
                {
                    return Err(ApiError::conflict(
                        "existing test run has different immutable content",
                    ));
                }
                let causal = test_run_causal_request(earlier)
                    .filter(|old| old.request_message_id == selector)
                    .ok_or_else(|| ApiError::conflict("ordinary run cannot adopt a later request"))?
                    .clone();
                if causal.runner != current_runner {
                    return Err(ApiError::conflict(
                        "retained test-request runner or store was replaced",
                    ));
                }
                let capture = TestRunCausalCapture {
                    causal,
                    project_id: earlier.project_id.clone().ok_or_else(|| {
                        ApiError::conflict("retained request project unavailable")
                    })?,
                    peer: None,
                };
                capture.validate_locked(&inner, session, run)?;
                return Ok(Some(capture));
            }
        }
        let selected = match prepared {
            Some(message) => message,
            None => self.selected_local_test_message(session, selector)?,
        };
        if selected.selector != selector || selected.message.id != selector {
            return Err(ApiError::conflict(
                "selected mailbox read returned another original message",
            ));
        }
        if selected.runner != current_runner {
            return Err(ApiError::conflict(
                "selected test-request runner or store changed during its read",
            ));
        }
        let payload = selected.message.test_request.ok_or_else(|| {
            ApiError::conflict("selected message has no host-owned typed test request")
        })?;
        validate_mailbox_test_payload(&payload)?;
        if payload.recipient.as_ref() != Some(&selected.runner)
            || ![
                selected.runner.native_session_id.as_str(),
                selected.runner.participant_key().as_str(),
            ]
            .contains(&selected.message.target_session_id.as_str())
            || ![
                payload.requester.native_session_id.as_str(),
                payload.requester.participant_key().as_str(),
            ]
            .contains(&selected.message.sender_session_id.as_str())
        {
            return Err(ApiError::conflict(
                "selected test request belongs to another native recipient or sender",
            ));
        }
        if !selected.message.idempotency_key.is_empty()
            && selected.message.idempotency_key != payload.request_send_key
        {
            return Err(ApiError::conflict(
                "selected request send key differs from retained message",
            ));
        }
        let expected_full = payload.mode == MailboxTestMode::Full;
        if request.get("full").and_then(Value::as_bool) != Some(expected_full) {
            return Err(ApiError::conflict(
                "selected test request requires its exact original full/focused mode",
            ));
        }
        if payload.targets.iter().any(|target| {
            matches!(
                &target.target.r#use,
                MailboxTestInputUse::CommittedInput { .. }
            )
        }) && !test_run_request_is_committed_input(request)
        {
            return Err(ApiError::conflict(
                "selected committed-input request requires committed launcher input",
            ));
        }
        // Actual tested commit/tree and qualification are settled from native
        // completion later. Registration invents no source measurement.
        let causal = TestRunCausalRequest {
            request_message_id: selector.to_owned(),
            canonical_message_id: selected.canonical,
            request_send_key: payload.request_send_key.clone(),
            runner: selected.runner,
            payload,
        };
        let capture = TestRunCausalCapture {
            causal,
            project_id: selected.project_id,
            peer: selected.peer,
        };
        let inner = self.inner.lock().expect("state mutex poisoned");
        capture.validate_locked(&inner, session, run)?;
        Ok(Some(capture))
    }
}
