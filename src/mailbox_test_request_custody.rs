// Host capture for one typed send. Ordinary mail keeps its existing constructors
// and path; custody is composed beside the send, then pinned in that same intent.

#[derive(Clone)]
struct MailboxTestSendCustody {
    payload: MailboxTestPayload,
    service: Option<EngramBindingTarget>,
    roots: Vec<EngramWorkSourceRoot>,
    sender_scope: Option<MailboxTestSenderScope>,
}

// Host-private destination mapping, retained on the sender only. This is
// never part of MailboxMessage, the relay payload, tool output or logs.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", try_from = "MailboxTestSenderScopeWire")]
struct MailboxTestSenderScope {
    project_id: String,
    store: EngramAuthorityStoreKey,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxTestSenderScopeWire {
    project_id: String,
    store: MailboxTestStoreWire,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MailboxTestStoreWire {
    database_path: PathBuf,
    project_id: String,
}

impl TryFrom<MailboxTestSenderScopeWire> for MailboxTestSenderScope {
    type Error = &'static str;
    fn try_from(value: MailboxTestSenderScopeWire) -> std::result::Result<Self, Self::Error> {
        if value.project_id.is_empty()
            || value.project_id.len() > 4096
            || value.store.project_id.is_empty()
            || value.store.project_id.len() > 4096
            || value.store.database_path.as_os_str().is_empty()
        {
            return Err("invalid host-private test-request destination mapping");
        }
        Ok(Self {
            project_id: value.project_id,
            store: EngramAuthorityStoreKey {
                database_path: value.store.database_path,
                project_id: value.store.project_id,
            },
        })
    }
}

impl std::fmt::Debug for MailboxTestSenderScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MailboxTestSenderScope(<host-private>)")
    }
}

fn validate_mailbox_test_sender_scope(scope: &MailboxTestSenderScope) -> Result<(), ApiError> {
    let invalid =
        || ApiError::bad_request("invalid_request: malformed host-private test-request mapping");
    let encoded = serde_json::to_vec(scope).map_err(|_| invalid())?;
    if encoded.len() > MAILBOX_TEST_REQUEST_MAX_BYTES {
        return Err(invalid());
    }
    serde_json::from_slice::<MailboxTestSenderScope>(&encoded).map_err(|_| invalid())?;
    Ok(())
}

fn mailbox_test_payload_matches_request(
    payload: Option<&MailboxTestPayload>,
    request: Option<&MailboxTestRequest>,
) -> bool {
    match (payload, request) {
        (None, None) => true,
        (Some(payload), Some(request)) => {
            payload.mode == request.mode
                && payload.targets.len() == request.targets.len()
                && payload
                    .targets
                    .iter()
                    .zip(&request.targets)
                    .all(|(a, b)| a.target == *b)
        }
        _ => false,
    }
}

impl MailboxTestSendCustody {
    fn for_recipient(&self, recipient: &MailboxRelayPrincipal) -> Result<Self, ApiError> {
        if self
            .payload
            .recipient
            .as_ref()
            .is_some_and(|original| original != recipient)
        {
            return Err(ApiError::conflict(
                "test-request recipient was replaced; original send stays pinned",
            ));
        }
        let mut captured = self.clone();
        captured.payload.recipient = Some(recipient.clone());
        validate_mailbox_test_payload(&captured.payload)?;
        Ok(captured)
    }

    // Runs while the ordinary native send admission guard holds StateInner.
    // All service I/O was completed before acquiring that guard.
    fn validate_locked(&self, inner: &StateInner, session: &str) -> Result<(), ApiError> {
        let record = federation_mailbox_root_locked(inner, session)?;
        if record.federation_native_creation_id != self.payload.requester.native_creation_id
            || session != self.payload.requester.native_session_id
            || inner.federation.local.as_ref() != Some(&self.payload.requester.host)
        {
            return Err(ApiError::conflict(
                "test-request sender identity changed before publication",
            ));
        }
        if let Some(service) = &self.service {
            let now = AppState::engram_source_root_target_locked(inner, session)
                .ok()
                .flatten()
                .ok_or_else(|| ApiError::conflict("test-request authority unavailable"))?;
            if now.connection != service.connection
                || self.sender_scope.as_ref().is_some_and(|scope| {
                    now.project_id != scope.project_id
                        || now.settings.authority_store_key.as_ref() != Some(&scope.store)
                })
                || !now.settings.same_work_service_settings(&service.settings)
                || now.external_ref != service.external_ref
                || !AppState::engram_root_runtime_is_current(record, &service.runtime_snapshot)
                || self.roots.iter().any(|root| {
                    !inner
                        .engram_work_source_roots
                        .iter()
                        .any(|current| current == root)
                })
            {
                return Err(ApiError::conflict(
                    "test-request authority or named root changed before publication",
                ));
            }
        }
        validate_mailbox_test_payload_draft(&self.payload)
    }
}

impl AppState {
    fn prepare_mailbox_test_request(
        &self,
        session: &str,
        key: &str,
        request: Option<&MailboxTestRequest>,
    ) -> Result<Option<MailboxTestSendCustody>, ApiError> {
        let Some(request) = request else {
            return Ok(None);
        };
        validate_mailbox_test_request(request)?;
        let retained = self
            .mailbox_store
            .retained_mailbox_test_payload(session, key)
            .map_err(mailbox_api_error)?;
        if let Some((payload, sender_scope)) = retained {
            if !mailbox_test_payload_matches_request(payload.as_ref(), Some(request)) {
                return Err(ApiError::conflict(
                    "test-request idempotency key belongs to its original metadata",
                ));
            }
            let payload = payload
                .ok_or_else(|| ApiError::conflict("ordinary mail cannot adopt a test request"))?;
            if self.native_mailbox_relay_descriptor(session)?.principal != payload.requester {
                return Err(ApiError::conflict("test-request sender was replaced"));
            }
            let captured = MailboxTestSendCustody {
                payload,
                service: None,
                roots: Vec::new(),
                sender_scope,
            };
            let inner = self.inner.lock().expect("state mutex poisoned");
            captured.validate_locked(&inner, session)?;
            return Ok(Some(captured));
        }
        self.capture_mailbox_test_request(session, key, request)
            .map(Some)
    }

    fn capture_mailbox_test_request(
        &self,
        session: &str,
        send_key: &str,
        request: &MailboxTestRequest,
    ) -> Result<MailboxTestSendCustody, ApiError> {
        validate_mailbox_test_request(request)?;
        let requester = self.native_mailbox_relay_descriptor(session)?.principal;
        let service = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            federation_mailbox_root_locked(&inner, session)?;
            Self::engram_source_root_target_locked(&inner, session)
                .ok()
                .flatten()
        };
        // The existing held-claim owner reads each item's current association;
        // focus and a caller-supplied binding never substitute for this read.
        let held = service.as_ref().and_then(|service| {
            service
                .adapter
                .read_held_claims(
                    &service.connection,
                    service
                        .settings
                        .call_timeout()
                        .min(ENGRAM_WORK_BINDING_COMMAND_TIMEOUT),
                )
                .ok()
        });
        let mut roots = Vec::new();
        let targets = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            request
                .targets
                .iter()
                .map(|target| {
                    let association = (|| {
                        let service = service.as_ref()?;
                        let store = service.settings.authority_store_key.as_ref()?;
                        let held = held.as_ref()?;
                        let mut matches = held.items.iter().filter(|claim| {
                            claim.work_id == target.work_ref || claim.short_ref == target.work_ref
                        });
                        let claim = matches.next()?;
                        if matches.next().is_some() {
                            return None;
                        }
                        let binding = claim.control_binding.as_ref()?;
                        if binding.work_id != claim.work_id
                            || binding.claim_id != claim.claim_id
                            || binding.claim_fence != claim.claim_fence
                        {
                            return None;
                        }
                        let mut matching_roots =
                            inner.engram_work_source_roots.iter().filter(|entry| {
                                &entry.store == store
                                    && entry.work_id == binding.work_id
                                    && entry.claim_id == binding.claim_id
                                    && entry.claim_fence == binding.claim_fence
                                    && test_run_held_claim_owns_name(&inner, entry, held)
                            });
                        let entry = matching_roots.next()?;
                        if matching_roots.next().is_some() {
                            return None;
                        }
                        let generation = i64::try_from(entry.generation)
                            .ok()
                            .filter(|generation| *generation > 0)?;
                        roots.push(entry.clone());
                        Some(MailboxTestAssociation {
                            binding: binding.clone(),
                            root: EngramVerificationRecordRoot {
                                workspace_id: entry.root.clone(),
                                generation,
                                state: EngramVerificationRecordRootState::Named,
                            },
                        })
                    })();
                    let uncredited_reason = association.is_none().then(|| {
                        if service.is_none() || held.is_none() {
                        "producer_unsupported: current held-claim service is unavailable"
                    } else {
                        "association_stale: target has no unique current held claim and named root"
                    }.to_owned()
                    });
                    MailboxTestCapturedTarget {
                        target: target.clone(),
                        association,
                        uncredited_reason,
                    }
                })
                .collect()
        };
        let sender_scope = service.as_ref().and_then(|service| {
            service
                .settings
                .authority_store_key
                .clone()
                .map(|store| MailboxTestSenderScope {
                    project_id: service.project_id.clone(),
                    store,
                })
        });
        let captured = MailboxTestSendCustody {
            payload: MailboxTestPayload {
                schema_version: 1,
                request_send_key: send_key.to_owned(),
                requester,
                recipient: None,
                mode: request.mode,
                targets,
            },
            service,
            roots,
            sender_scope,
        };
        let inner = self.inner.lock().expect("state mutex poisoned");
        captured.validate_locked(&inner, session)?;
        Ok(captured)
    }
}

impl MailboxStore {
    fn retained_mailbox_test_payload(
        &self,
        sender: &str,
        key: &str,
    ) -> Result<Option<(Option<MailboxTestPayload>, Option<MailboxTestSenderScope>)>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT test_request_json,test_request_scope_json FROM mailbox_messages WHERE sender_session_id=?1 AND idempotency_key=?2
             UNION ALL SELECT test_request_json,test_request_scope_json FROM mailbox_relay_intents WHERE owner_session_id=?1 AND idempotency_key=?2 LIMIT 2")?;
        let rows = statement
            .query_map(rusqlite::params![sender, key], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.len() > 1 {
            bail!("ambiguous original mailbox custody; no route may be selected");
        }
        rows.into_iter()
            .next()
            .map(|(payload, scope)| {
                if payload
                    .as_ref()
                    .is_some_and(|encoded| encoded.len() > MAILBOX_TEST_REQUEST_MAX_BYTES)
                    || scope
                        .as_ref()
                        .is_some_and(|encoded| encoded.len() > MAILBOX_TEST_REQUEST_MAX_BYTES)
                {
                    bail!("retained test-request custody exceeds its bound");
                }
                let payload = payload
                    .map(|encoded| serde_json::from_str(&encoded))
                    .transpose()?;
                let scope = scope
                    .map(|encoded| serde_json::from_str(&encoded))
                    .transpose()?;
                if payload.is_none() && scope.is_some() {
                    bail!("orphaned sender test-request scope");
                }
                Ok((payload, scope))
            })
            .transpose()
    }
}

fn mailbox_test_payload_from_row(
    row: &rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<MailboxTestPayload>> {
    let encoded: Option<String> = row.get(column)?;
    encoded
        .map(|encoded| {
            if encoded.len() > MAILBOX_TEST_REQUEST_MAX_BYTES {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    column,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "test-request custody exceeds its bound",
                    )),
                ));
            }
            serde_json::from_str(&encoded).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    column,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn mailbox_test_scope_from_row(
    row: &rusqlite::Row<'_>,
    column: usize,
) -> rusqlite::Result<Option<MailboxTestSenderScope>> {
    let encoded: Option<String> = row.get(column)?;
    encoded
        .map(|encoded| {
            if encoded.len() > MAILBOX_TEST_REQUEST_MAX_BYTES {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    column,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "private test-request mapping exceeds its bound",
                    )),
                ));
            }
            serde_json::from_str(&encoded).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    column,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}
