// Owns sender-local status, exact retry and terminal payload cleanup routes.
// Does not route status/read/ACK to a cache or grant authority from an intent.
// The native creation remains the local owner even while federation is held.
#[derive(Clone)]
struct FederationDeliveryCapability {
    central: FederationHostIdentity,
    lease: Option<FederationLinkLease>,
    supported: bool,
}

impl FederationDeliveryCapability {
    // Acquired after writer/connection admission and retained through COMMIT.
    // A reply from a retired link cannot authorize a new durable sender slot.
    fn lock<'a>(
        &'a self,
        app: &'a AppState,
        session: &str,
        creation: &str,
        sender: &FederationMailboxBinding,
    ) -> Result<FederationMailboxAdmission<'a>> {
        let link = self.lease.as_ref().map(|lease| {
            let link = lease.channel.state.lock().expect("federation link mutex poisoned");
            if link.incarnation.as_deref() != Some(lease.incarnation.as_str())
                || link.recorded_incarnation.as_deref() != Some(lease.incarnation.as_str()) {
                return Err(federation_delivery_conflict());
            }
            Ok(link)
        }).transpose()?;
        let inner = app.lock_federation_intent_owner(session, creation, sender)?;
        if self.central != sender.central {
            return Err(federation_delivery_conflict());
        }
        if let Some(lease) = &self.lease {
            let basis = lease.channel.transport_basis();
            let peer = app.refresh_federation_transport_snapshot_locked(
                &inner, &basis, &lease.incarnation,
            ).map_err(|_| federation_delivery_conflict())?;
            if inner.preferences.federation.role != FederationRole::Satellite
                || peer.peer != self.central {
                return Err(federation_delivery_conflict());
            }
        } else if inner.preferences.federation.role != FederationRole::Central
            || inner.federation.local.as_ref() != Some(&self.central) {
            return Err(federation_delivery_conflict());
        }
        Ok(FederationMailboxAdmission { _inner: inner, _link: link })
    }
}

impl AppState {
    async fn negotiate_federation_delivery_capability(
        &self,
        session: &str,
    ) -> Result<FederationDeliveryCapability, ApiError> {
        let (protocol, lease): (FederationMailboxProtocol, _) = self
            .dispatch_federation_mailbox_operation_with_capability(
                session, FederationMailboxOperation::Protocol, None,
            ).await?;
        if protocol.protocol_version != FEDERATION_MAILBOX_WIRE_VERSION
            || protocol.schema_version != FEDERATION_MAILBOX_SCHEMA_VERSION {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(FederationDeliveryCapability { central: protocol.central, lease,
            supported: protocol.sender_intent_registry })
    }

    async fn refuse_unsupported_federation_delivery(
        &self,
        session: &str,
        delivery: &FederationDelivery,
        capability: FederationDeliveryCapability,
    ) -> Result<ApiError, ApiError> {
        let app = self.clone();
        let creation = self.federation_intent_owner(session)?;
        let native = session.to_owned();
        let delivery = delivery.clone();
        let refusal = run_blocking_api(move || {
            app.mailbox_store.refuse_unattempted_federation_intent(&delivery, &native, &creation, || {
                capability.lock(&app, &native, &creation, &delivery.sender)
            }).map_err(mailbox_api_error)
        }).await?;
        Ok(if refusal.is_some() { federation_delivery_local_refusal() }
            else { federation_delivery_unsupported() })
    }

    async fn dispatch_federation_delivery_operation<T: DeserializeOwned>(
        &self,
        session: &str,
        operation: FederationMailboxOperation,
        capability: &FederationDeliveryCapability,
    ) -> Result<T, ApiError> {
        let delivery = match &operation {
            FederationMailboxOperation::SendExact { delivery }
            | FederationMailboxOperation::CancelExact { delivery } => delivery,
            _ => return Err(ApiError::bad_request("exact delivery operation required")),
        };
        if !capability.supported {
            return Err(federation_delivery_unsupported());
        }
        let creation = self.federation_intent_owner(session)?;
        {
            let _current = capability.lock(self, session, &creation, &delivery.sender)
                .map_err(mailbox_api_error)?;
        }
        self.dispatch_federation_mailbox_operation_with_capability(
            session, operation, Some(capability),
        ).await.map(|(value, _)| value)
    }

    fn federation_intent_owner(&self, session: &str) -> Result<String, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        if !inner.preferences.federation.enabled {
            return Err(ApiError::not_found("domain delivery intents are disabled"));
        }
        Ok(federation_mailbox_root_locked(&inner, session)?
            .federation_native_creation_id
            .clone())
    }

    fn lock_federation_intent_owner<'a>(
        &'a self,
        session: &str,
        creation: &str,
        binding: &FederationMailboxBinding,
    ) -> Result<StateMutexGuard<'a, StateInner>> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let valid = federation_mailbox_root_locked(&inner, session).is_ok_and(|record| {
            record.federation_native_creation_id == creation
                && record.federation_mailbox_binding.as_ref() == Some(binding)
        });
        if !valid
            || !inner.preferences.federation.enabled
            || !inner.federation_durable
            || inner.federation_continuity_lost
            || inner.federation.admission_quarantine.is_some()
            || inner.preferences.federation.central.as_ref() != Some(&binding.central)
            || inner.federation.local.as_ref().map(|local| &local.host_id)
                != Some(&binding.identity.host_id)
        {
            return Err(federation_delivery_conflict());
        }
        Ok(inner)
    }

    async fn prepare_federation_delivery(
        &self,
        session: &str,
        operation: FederationMailboxOperation,
    ) -> Result<FederationDelivery, ApiError> {
        self.prepare_federation_delivery_with_target(session, operation, None).await
    }

    async fn prepare_federation_delivery_with_target(
        &self,
        session: &str,
        operation: FederationMailboxOperation,
        captured_target: Option<FederationMailboxBinding>,
    ) -> Result<FederationDelivery, ApiError> {
        let FederationMailboxOperation::Send {
            target,
            message,
            key,
            topic,
            state_stamp,
        } = operation
        else {
            return Err(ApiError::bad_request(
                "delivery preparation requires a send",
            ));
        };
        let actor = self
            .federation_mailbox_actor(session)?
            .ok_or_else(|| ApiError::conflict("session has no explicit central mailbox binding"))?;
        let creation = self.federation_intent_owner(session)?;
        let role = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .preferences
            .federation
            .role;
        // Both sender roles may retry an exact retained binding after target
        // retirement. Blocking SQLite always runs off the async executor, and
        // its connection is released before authoritative alias discovery.
        let lookup_store = self.mailbox_store.clone();
        let lookup_sender = actor.binding.participant_key();
        let lookup_key = key.clone();
        let retained = run_blocking_api(move || {
            let connection = lookup_store.connection().map_err(mailbox_api_error)?;
            federation_intent_read(&connection, &lookup_sender, &lookup_key)
                .map_err(mailbox_api_error)
        }).await?;
        let exact = retained.as_ref().map(|retained| {
            retained.matches_owner(session, &creation).map_err(mailbox_api_error)?;
            retained.identity().map_err(mailbox_api_error)
        }).transpose()?;
        if let Some(captured) = &captured_target {
            captured.validate()?;
            if target != captured.participant_key() || captured.central != actor.binding.central
                || exact.as_ref().is_some_and(|retained| retained.target != *captured) {
                return Err(mailbox_api_error(federation_delivery_conflict()));
            }
        }
        let target = match exact {
            Some(exact) if target == exact.target.participant_key() => FederationMailboxParticipant {
                session_id: exact.target.participant_key(), binding: exact.target,
                host_name: "Retained target".into(), name: "Retained target".into(),
            },
            _ => if let Some(binding) = captured_target {
                FederationMailboxParticipant { session_id: binding.participant_key(), binding,
                    host_name: "Captured target".into(), name: "Captured target".into() }
            } else { self.resolve_delivery_recipient(session, role, &target).await? },
        };
        let delivery = FederationDelivery {
            version: 1,
            sender: actor.binding,
            target: target.binding,
            key,
            message,
            topic,
            state_stamp,
        };
        delivery.validate()?;
        if let Some(retained) = retained {
            if retained.digest != delivery.digest() {
                return Err(mailbox_api_error(federation_delivery_conflict()));
            }
            if let Some(error) = retained.terminal_error() { return Err(error); }
            if retained.state == "homeCommitted" {
                let _current = self.lock_federation_intent_owner(session, &creation, &delivery.sender)
                    .map_err(mailbox_api_error)?;
                return Ok(delivery);
            }
        }
        let offline_basis = if role == FederationRole::Satellite {
            Some(self.federation_peer_snapshot(&delivery.sender.central.host_id)?)
        } else { None };
        let capability = match self.negotiate_federation_delivery_capability(session).await {
            Ok(proof) if !proof.supported => {
                return Err(self.refuse_unsupported_federation_delivery(session, &delivery, proof).await?);
            }
            Ok(proof) => Some(proof),
            // A local retry queue is not central coordination admission. Its
            // exact native/binding authority is still fenced through COMMIT.
            Err(error) if error.status == StatusCode::SERVICE_UNAVAILABLE => None,
            Err(error) => return Err(error),
        };
        let app = self.clone();
        let session = session.to_owned();
        let stored = delivery.clone();
        run_blocking_api(move || {
            app.mailbox_store
                .prepare_federation_intent(&stored, &session, &creation, || {
                    if let Some(proof) = &capability {
                        proof.lock(&app, &session, &creation, &stored.sender)
                    } else {
                        let inner = app.lock_federation_intent_owner(&session, &creation, &stored.sender)?;
                        if inner.preferences.federation.role != FederationRole::Satellite {
                            return Err(federation_delivery_conflict());
                        }
                        let basis = offline_basis.as_ref().ok_or_else(federation_delivery_conflict)?;
                        // The satellite records its central as Enrolled after
                        // connecting. This local queue grants no new central
                        // admission; the existing binding must remain current.
                        if basis.membership == FederationMembershipState::Revoked {
                            return Err(federation_delivery_conflict());
                        }
                        app.ensure_federation_peer_current_locked(&inner, basis)
                            .map_err(|_| federation_delivery_conflict())?;
                        Ok(FederationMailboxAdmission { _inner: inner, _link: None })
                    }
                })
                .map_err(mailbox_api_error)?;
            Ok(())
        })
        .await?;
        Ok(delivery)
    }

    async fn resolve_delivery_recipient(
        &self,
        session: &str,
        role: FederationRole,
        reference: &str,
    ) -> Result<FederationMailboxParticipant, ApiError> {
        if role == FederationRole::Central {
            return resolve_federation_mailbox_participant_locked(
                &self.inner.lock().expect("state mutex poisoned"), reference,
            );
        }
        let rows: Vec<FederationMailboxParticipant> = self.dispatch_federation_mailbox_operation(
            session, FederationMailboxOperation::Directory,
        ).await?;
        resolve_federation_delivery_target(&rows, reference)
    }

    async fn send_prepared_federation_delivery(
        &self,
        session: &str,
        delivery: FederationDelivery,
    ) -> Result<MailboxAppendReceipt, ApiError> {
        let creation = self.federation_intent_owner(session)?;
        let app = self.clone();
        let native = session.to_owned();
        let settle_native = native.clone();
        let settle_creation = creation.clone();
        let stored = delivery.clone();
        let committed = run_blocking_api(move || {
            let intent = {
                let connection = app.mailbox_store.connection().map_err(mailbox_api_error)?;
                let intent = federation_intent_read(
                    &connection,
                    &stored.sender.participant_key(),
                    &stored.key,
                )
                .map_err(mailbox_api_error)?
                .ok_or_else(|| ApiError::not_found("delivery intent not found"))?;
                intent
                    .matches_owner(&native, &creation)
                    .map_err(mailbox_api_error)?;
                if intent.digest != stored.digest() {
                    return Err(mailbox_api_error(federation_delivery_conflict()));
                }
                if let Some(error) = intent.terminal_error() { return Err(error); }
                intent
            };
            if let Some(receipt) = intent.committed_receipt().map_err(mailbox_api_error)? {
                let _current = app
                    .lock_federation_intent_owner(&native, &creation, &stored.sender)
                    .map_err(mailbox_api_error)?;
                return Ok(Some(receipt));
            }
            Ok(None)
        })
        .await?;
        if let Some(receipt) = committed {
            return Ok(receipt);
        }
        let capability = self.negotiate_federation_delivery_capability(session).await.map_err(|mut error| {
            if error.status == StatusCode::SERVICE_UNAVAILABLE {
                eprintln!("federation delivery capability unavailable: {}", error.message);
                error.message = "central delivery capability unavailable; retained sender intent is unchanged; this request made no append attempt".to_owned();
            }
            error
        })?;
        if !capability.supported {
            return Err(self.refuse_unsupported_federation_delivery(session, &delivery, capability).await?);
        }
        let app = self.clone();
        let native = session.to_owned();
        let creation = settle_creation.clone();
        let stored = delivery.clone();
        let admission = capability.clone();
        run_blocking_api(move || {
            app.mailbox_store.mark_federation_intent_unknown(&stored, || {
                admission.lock(&app, &native, &creation, &stored.sender)
            }).map_err(mailbox_api_error)
        }).await?;
        // Failure/lost reply leaves appendOutcomeUnknown durably intact. A
        // negative lookup or a local timeout never certifies a terminal state.
        let response: FederationDeliveryOutcome = self
            .dispatch_federation_delivery_operation(
                session,
                FederationMailboxOperation::SendExact {
                    delivery: delivery.clone(),
                },
                &capability,
            )
            .await?;
        let receipt = match response {
            FederationDeliveryOutcome::Committed(receipt) => receipt,
            FederationDeliveryOutcome::Terminal(terminal) => {
                if terminal.canonical_digest != delivery.digest() {
                    return Err(ApiError::bad_gateway("invalid central terminal outcome identity"));
                }
                let app = self.clone();
                run_blocking_api(move || {
                    app.mailbox_store.settle_federation_terminal_intent(&delivery, terminal.state, || {
                        app.lock_federation_intent_owner(&settle_native, &settle_creation, &delivery.sender)
                    }).map_err(mailbox_api_error)
                }).await?;
                return Err(ApiError::conflict(format!(
                    "central delivery is {}; authoritative outcome retained", terminal.state.as_str(),
                )));
            }
        };
        let app = self.clone();
        run_blocking_api(move || {
            app.mailbox_store
                .settle_federation_intent(&delivery, &receipt)
                .map_err(federation_delivery_settlement_error)?;
            Ok(receipt)
        })
        .await
    }

    fn local_federation_intent(
        &self,
        session: &str,
        key: &str,
    ) -> Result<FederationIntent, ApiError> {
        let creation = self.federation_intent_owner(session)?;
        let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
        let mut statement = connection
            .prepare(
                "SELECT sender_session_id FROM federation_mailbox_intents
             WHERE owner_session_id=?1 AND owner_creation_id=?2 AND idempotency_key=?3",
            )
            .map_err(|error| mailbox_api_error(error.into()))?;
        let senders = statement
            .query_map(rusqlite::params![session, creation, key], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| mailbox_api_error(error.into()))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|error| mailbox_api_error(error.into()))?;
        if senders.len() > 1 {
            return Err(ApiError::conflict(
                "delivery key has multiple retained authority epochs",
            ));
        }
        let sender = senders
            .first()
            .ok_or_else(|| ApiError::not_found("delivery intent not found"))?;
        let result = federation_intent_read(&connection, sender, key)
            .map_err(mailbox_api_error)?
            .ok_or_else(|| ApiError::not_found("delivery intent not found"))?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        if !federation_mailbox_root_locked(&inner, session)
            .is_ok_and(|record| record.federation_native_creation_id == creation)
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        Ok(result)
    }
}

fn resolve_federation_delivery_target(
    rows: &[FederationMailboxParticipant],
    reference: &str,
) -> Result<FederationMailboxParticipant, ApiError> {
    if let Some(exact) = rows.iter().find(|row| row.session_id == reference) {
        exact.validate()?;
        return Ok(exact.clone());
    }
    // Canonical ids and bare native ids never fall through to display aliases.
    if reference.starts_with("domain-session-") || reference.starts_with("session-") {
        return Err(ApiError::not_found(
            "qualified id required; use the directory sessionId or exact Host::Name",
        ));
    }
    let mut matches = rows.iter().filter(|row| {
        row.name.eq_ignore_ascii_case(reference)
            || format!("{}::{}", row.host_name, row.name).eq_ignore_ascii_case(reference)
            || format!("{}::{}", row.binding.identity.host_id, row.name)
                .eq_ignore_ascii_case(reference)
    });
    let first = matches
        .next()
        .ok_or_else(|| ApiError::not_found("qualified mailbox participant not found"))?;
    if matches.next().is_some() {
        return Err(ApiError::bad_request(
            "mailbox participant alias is ambiguous",
        ));
    }
    first.validate()?;
    Ok(first.clone())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrepareFederationIntentRequest {
    target_session_id: String,
    message: String,
    idempotency_key: String,
    topic: Option<String>,
    state_stamp: Option<String>,
    class: Option<String>,
    target_binding: Option<FederationMailboxBinding>,
}

#[derive(Deserialize)]
struct SendMailboxApiRequest {
    #[serde(flatten)]
    message: SendMailboxMessageRequest,
    #[serde(default, rename = "targetBinding", deserialize_with = "reject_prepare_target_binding")]
    _target_binding: (),
}

fn reject_prepare_target_binding<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    let _ = serde::de::IgnoredAny::deserialize(deserializer)?;
    Err(serde::de::Error::custom("targetBinding is only supported by local intent preparation"))
}

async fn prepare_federation_intent_route(
    AxumPath(session): AxumPath<String>,
    State(state): State<AppState>,
    Json(request): Json<PrepareFederationIntentRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if request
        .class
        .as_deref()
        .is_some_and(|class| class != "routine")
    {
        return Err(ApiError::bad_request(
            "only routine delivery intents are supported",
        ));
    }
    let delivery = state
        .prepare_federation_delivery_with_target(
            &session,
            FederationMailboxOperation::Send {
                target: request.target_session_id,
                message: request.message,
                key: request.idempotency_key,
                topic: request.topic,
                state_stamp: request.state_stamp,
            },
            request.target_binding,
        )
        .await?;
    let value = run_blocking_api(move || {
        state
            .local_federation_intent(&session, &delivery.key)
            .map(|intent| intent.status())
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(value)))
}

async fn get_federation_intent_route(
    AxumPath((session, key)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    run_blocking_api(move || {
        state
            .local_federation_intent(&session, &key)
            .map(|intent| Json(intent.status()))
    })
    .await
}

async fn retry_federation_intent_route(
    AxumPath((session, key)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<MailboxAppendReceipt>, ApiError> {
    let app = state.clone();
    let native = session.clone();
    let intent = run_blocking_api(move || app.local_federation_intent(&native, &key)).await?;
    if let Some(receipt) = intent.committed_receipt().map_err(mailbox_api_error)? {
        let identity = intent.identity().map_err(mailbox_api_error)?;
        let _current = state
            .lock_federation_intent_owner(&session, &intent.creation, &identity.sender)
            .map_err(mailbox_api_error)?;
        return Ok(Json(receipt));
    }
    if let Some(error) = intent.terminal_error() { return Err(error); }
    let payload = intent
        .payload
        .ok_or_else(|| ApiError::conflict("delivery retry payload was collected"))?;
    let delivery: FederationDelivery = serde_json::from_str(&payload)
        .map_err(|_| ApiError::internal("invalid durable delivery payload"))?;
    state
        .send_prepared_federation_delivery(&session, delivery)
        .await
        .map(Json)
}

async fn cancel_federation_intent_route(
    AxumPath((session, key)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let app = state.clone();
    let native = session.clone();
    let intent = run_blocking_api(move || app.local_federation_intent(&native, &key)).await?;
    if intent.state == "abandoned" {
        return Ok(Json(intent.status()));
    }
    if let Some(error) = intent.terminal_error() { return Err(error); }
    if intent.state != "localIntentDurable" {
        return Err(ApiError::conflict(
            "possibly sent or committed delivery cannot be cancelled from a negative lookup",
        ));
    }
    let payload = intent
        .payload
        .as_deref()
        .ok_or_else(|| ApiError::conflict("delivery payload unavailable"))?;
    let delivery: FederationDelivery = serde_json::from_str(payload)
        .map_err(|_| ApiError::internal("invalid durable delivery payload"))?;
    let confirmation: FederationDeliveryTerminal = state
        .dispatch_federation_mailbox_operation(
            &session,
            FederationMailboxOperation::CancelExact {
                delivery: delivery.clone(),
            },
        )
        .await?;
    if confirmation.state != FederationDeliveryTerminalState::Abandoned
        || confirmation.canonical_digest != delivery.digest()
    {
        return Err(ApiError::bad_gateway(
            "invalid central cancellation outcome",
        ));
    }
    run_blocking_api(move || {
        state.mailbox_store.settle_federation_terminal_intent(
            &delivery, FederationDeliveryTerminalState::Abandoned, || {
                state.lock_federation_intent_owner(&session, &intent.creation, &delivery.sender)
            },
        ).map_err(mailbox_api_error)?;
        state.local_federation_intent(&session,&intent.key).map(|row|Json(row.status()))
    }).await
}

async fn gc_federation_intent_route(
    AxumPath((session, key)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    run_blocking_api(move || {
        let intent = state.local_federation_intent(&session, &key)?;
        if !matches!(
            intent.state.as_str(),
            "homeCommitted" | "refused" | "abandoned"
        ) {
            return Err(ApiError::conflict(
                "pending or unknown delivery payload cannot be collected",
            ));
        }
        let _writer = state
            .mailbox_store
            .lock_writer("collecting terminal retry payload")
            .map_err(mailbox_api_error)?;
        let mut connection = state
            .mailbox_store
            .connection()
            .map_err(mailbox_api_error)?;
        let transaction = begin_mailbox_write(&mut connection, "collecting terminal retry payload")
            .map_err(mailbox_api_error)?;
        transaction
            .execute(
                "UPDATE federation_mailbox_intents SET payload=NULL,payload_bytes=0
             WHERE sender_session_id=?1 AND idempotency_key=?2 AND owner_creation_id=?3
             AND state IN ('homeCommitted','refused','abandoned')",
                rusqlite::params![intent.sender, intent.key, intent.creation],
            )
            .map_err(|error| mailbox_api_error(error.into()))?;
        let inner = state.inner.lock().expect("state mutex poisoned");
        if !federation_mailbox_root_locked(&inner, &session)
            .is_ok_and(|record| record.federation_native_creation_id == intent.creation)
        {
            return Err(ApiError::conflict(FEDERATION_STALE_AUTHORITY));
        }
        transaction
            .commit()
            .map_err(|error| mailbox_api_error(error.into()))?;
        Ok(Json(intent.status()))
    })
    .await
}
