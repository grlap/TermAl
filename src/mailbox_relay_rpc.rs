// Authenticated native mailbox operations; principal claims originate on their host.
// Owns no provider/executor entry point and performs no network I/O under state/SQL locks.

struct MailboxRelayAuthority {
    app: AppState,
    lease: FederationLinkLease,
    native: Vec<MailboxRelayPrincipal>,
    home: Option<MailboxRelayHome>,
}

impl MailboxRelayAuthority {
    fn lock(&self) -> Result<FederationMailboxAdmission<'_>> {
        let link = self
            .lease
            .channel
            .state
            .lock()
            .expect("federation link mutex poisoned");
        if link.incarnation.as_deref() != Some(self.lease.incarnation.as_str())
            || link.recorded_incarnation.as_deref() != Some(self.lease.incarnation.as_str())
        {
            return Err(relay_store_error("native mailbox relay link changed"));
        }
        let inner = self.app.inner.lock().expect("state mutex poisoned");
        let peer = self
            .app
            .refresh_federation_transport_snapshot_locked(
                &inner,
                &self.lease.channel.transport_basis(),
                &self.lease.incarnation,
            )
            .map_err(|error| relay_store_error(&error.message))?;
        if self.home.as_ref().is_some_and(|home| {
            home.host != peer.local || peer.local_role != FederationRole::Central
        }) {
            return Err(relay_store_error("native mailbox home authority changed"));
        }
        for principal in &self.native {
            let root = federation_mailbox_root_locked(&inner, &principal.native_session_id)
                .map_err(|error| relay_store_error(&error.message))?;
            if principal.host != peer.local
                || root.federation_native_creation_id != principal.native_creation_id
            {
                return Err(relay_store_error("native mailbox session was replaced"));
            }
        }
        Ok(FederationMailboxAdmission {
            _inner: inner,
            _link: Some(link),
        })
    }
}

fn mailbox_relay_rpc_routes() -> Router<FederationListenerState> {
    Router::new()
        .route(MAILBOX_RELAY_RPC_ROUTE, post(serve_mailbox_relay))
        .layer(DefaultBodyLimit::max(FEDERATION_MAILBOX_REQUEST_BYTES))
}

async fn serve_mailbox_relay(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(request): Json<MailboxRelayRequest>,
) -> Result<Json<MailboxRelayResponse>, ApiError> {
    if request.version != MAILBOX_RELAY_VERSION || !mailbox_relay_uuid(&request.request_id) {
        return Err(ApiError::conflict("native mailbox relay upgrade required"));
    }
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let app = lease
        .channel
        .app
        .clone()
        .ok_or_else(federation_transport_unavailable)?;
    let origin = lease.with_current(&app, |_, current| Ok(current.peer.clone()))?;
    let permit = peer.mailbox_permit;
    run_blocking_api(move || {
        let _permit = permit;
        let value = app.execute_mailbox_relay(request.operation, &lease, &origin)?;
        let host = lease.with_current(&app, |_, current| Ok(current.local.clone()))?;
        Ok(Json(MailboxRelayResponse {
            version: MAILBOX_RELAY_VERSION,
            request_id: request.request_id,
            host,
            value,
        }))
    })
    .await
}

impl AppState {
    fn native_mailbox_relay_descriptor(
        &self,
        session: &str,
    ) -> Result<MailboxRelayDescriptor, ApiError> {
        let store_id = {
            let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
            mailbox_relay_store_id(&connection).map_err(mailbox_api_error)?
        };
        let inner = self.inner.lock().expect("state mutex poisoned");
        let root = federation_mailbox_root_locked(&inner, session)?;
        let host = inner
            .federation
            .local
            .clone()
            .ok_or_else(federation_transport_unavailable)?;
        let principal = MailboxRelayPrincipal {
            host,
            store_id,
            native_session_id: session.to_owned(),
            native_creation_id: root.federation_native_creation_id.clone(),
        };
        principal.validate()?;
        Ok(MailboxRelayDescriptor {
            principal,
            name: federation_mailbox_display_name(&root.session.name),
        })
    }

    fn native_mailbox_relay_home(&self) -> Result<MailboxRelayHome, ApiError> {
        let store_id = {
            let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
            mailbox_relay_store_id(&connection).map_err(mailbox_api_error)?
        };
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(MailboxRelayHome {
            host: federation_mailbox_central_locked(&inner)?,
            store_id,
        })
    }

    fn mailbox_relay_home_authority(
        &self,
        lease: &FederationLinkLease,
        home: &MailboxRelayHome,
        actors: &[MailboxRelayPrincipal],
    ) -> Result<MailboxRelayAuthority, ApiError> {
        home.validate()?;
        if *home != self.native_mailbox_relay_home()? {
            return Err(ApiError::conflict(
                "native mailbox home/store changed; no fallback",
            ));
        }
        let native = actors
            .iter()
            .filter(|actor| actor.host == home.host)
            .cloned()
            .collect::<Vec<_>>();
        if native.iter().any(|actor| actor.store_id != home.store_id) {
            return Err(ApiError::conflict(
                "native mailbox principal belongs to a replaced store",
            ));
        }
        Ok(MailboxRelayAuthority {
            app: self.clone(),
            lease: lease.clone(),
            native,
            home: Some(home.clone()),
        })
    }

    fn require_mailbox_relay_conversation(
        &self,
        actor: &MailboxRelayPrincipal,
        mailbox: &str,
        conversation: &MailboxRelayConversation,
    ) -> Result<(), ApiError> {
        let actual = self
            .mailbox_store
            .mailbox_relay_association(actor, mailbox)
            .map_err(mailbox_api_error)?;
        if actual.as_ref() != Some(conversation) {
            return Err(ApiError::not_found(
                "native mailbox conversation membership not found",
            ));
        }
        Ok(())
    }

    fn execute_mailbox_relay(
        &self,
        operation: MailboxRelayOperation,
        lease: &FederationLinkLease,
        authenticated_origin: &FederationHostIdentity,
    ) -> Result<Value, ApiError> {
        let value = match operation {
            MailboxRelayOperation::Describe { native_session_id } => {
                let descriptor = self.native_mailbox_relay_descriptor(&native_session_id)?;
                let authority = MailboxRelayAuthority {
                    app: self.clone(),
                    lease: lease.clone(),
                    native: vec![descriptor.principal.clone()],
                    home: None,
                };
                let _guard = authority.lock().map_err(mailbox_api_error)?;
                serde_json::to_value(descriptor)
            }
            MailboxRelayOperation::Send { append } => {
                append.sender.require_origin(authenticated_origin)?;
                append.input()?;
                let authority = self.mailbox_relay_home_authority(
                    lease,
                    &append.conversation.home,
                    &[append.sender.clone(), append.target.clone()],
                )?;
                let receipt = self
                    .mailbox_store
                    .append_mailbox_relay(&append, &lease.with_current(self, |_, peer|Ok(peer.clone()))?, || authority.lock())
                    .map_err(mailbox_api_error)?;
                self.federation_executor_wake_signal.kick();
                serde_json::to_value(receipt)
            }
            MailboxRelayOperation::List { actor, home } => {
                actor.require_origin(authenticated_origin)?;
                let authority =
                    self.mailbox_relay_home_authority(lease, &home, std::slice::from_ref(&actor))?;
                let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
                let rows = mailbox_summaries_for_session_with_empty(
                    &connection,
                    &actor.participant_key(),
                    true,
                )
                .map_err(mailbox_api_error)?;
                drop(connection);
                let rows = rows
                    .into_iter()
                    .map(|summary| {
                        let conversation = self
                            .mailbox_store
                            .mailbox_relay_association(&actor, &summary.id)
                            .map_err(mailbox_api_error)?
                            .ok_or_else(|| {
                                ApiError::conflict("native mailbox home association missing")
                            })?;
                        Ok(MailboxRelaySummary {
                            conversation,
                            summary,
                        })
                    })
                    .collect::<Result<Vec<_>, ApiError>>()?;
                let _guard = authority.lock().map_err(mailbox_api_error)?;
                serde_json::to_value(rows)
            }
            MailboxRelayOperation::Read {
                actor,
                conversation,
                mailbox_id,
                after_sequence,
                limit,
                issue_receipt,
            } => {
                actor.require_origin(authenticated_origin)?;
                self.require_mailbox_relay_conversation(&actor, &mailbox_id, &conversation)?;
                let authority = self.mailbox_relay_home_authority(
                    lease,
                    &conversation.home,
                    std::slice::from_ref(&actor),
                )?;
                let mut page = self
                    .mailbox_store
                    .read_range_authorized(
                        &actor.participant_key(),
                        &mailbox_id,
                        after_sequence,
                        limit,
                        issue_receipt,
                        || authority.lock(),
                    )
                    .map_err(mailbox_api_error)?;
                page.reader_session_id = Some(actor.participant_key());
                serde_json::to_value(page)
            }
            MailboxRelayOperation::ReadMessage {
                actor,
                home,
                message_id,
            } => {
                actor.require_origin(authenticated_origin)?;
                let message = self
                    .mailbox_store
                    .read_message(&actor.participant_key(), &message_id)
                    .map_err(mailbox_api_error)?;
                let conversation = self
                    .mailbox_store
                    .mailbox_relay_association(&actor, &message.mailbox_id)
                    .map_err(mailbox_api_error)?
                    .ok_or_else(|| ApiError::not_found("native mailbox association not found"))?;
                conversation.require_home(&home)?;
                let authority =
                    self.mailbox_relay_home_authority(lease, &home, std::slice::from_ref(&actor))?;
                let _guard = authority.lock().map_err(mailbox_api_error)?;
                serde_json::to_value(message)
            }
            MailboxRelayOperation::Acknowledge {
                actor,
                conversation,
                mailbox_id,
                receipt,
            } => {
                actor.require_origin(authenticated_origin)?;
                self.require_mailbox_relay_conversation(&actor, &mailbox_id, &conversation)?;
                let authority = self.mailbox_relay_home_authority(
                    lease,
                    &conversation.home,
                    std::slice::from_ref(&actor),
                )?;
                let summary = self
                    .mailbox_store
                    .acknowledge_authorized(
                        &actor.participant_key(),
                        &mailbox_id,
                        Some(&receipt),
                        0,
                        0,
                        || authority.lock(),
                    )
                    .map_err(mailbox_api_error)?;
                serde_json::to_value(summary)
            }
            MailboxRelayOperation::Wake { wake } => {
                wake.validate()?;
                if wake.conversation.home.host != *authenticated_origin {
                    return Err(ApiError::conflict(
                        "native mailbox wake did not originate at its home",
                    ));
                }
                lease.with_current(self, |_, peer| {
                    if peer.central != wake.conversation.home.host {
                        return Err(ApiError::conflict(
                            "native mailbox wake does not name the enrolled central home",
                        ));
                    }
                    Ok(())
                })?;
                let actual =
                    self.native_mailbox_relay_descriptor(&wake.target.native_session_id)?;
                if actual.principal != wake.target {
                    return Err(ApiError::conflict(
                        "native mailbox wake target was replaced",
                    ));
                }
                let authority = MailboxRelayAuthority {
                    app: self.clone(),
                    lease: lease.clone(),
                    native: vec![wake.target.clone()],
                    home: None,
                };
                let names = wake.conversation.participants.clone().map(|actor| {
                    if actor == wake.sender {
                        wake.sender_name.clone()
                    } else {
                        actual.name.clone()
                    }
                });
                self.mailbox_store
                    .remember_mailbox_relay(&wake.conversation, &wake.mailbox_id, &names, || {
                        authority.lock()
                    })
                    .map_err(mailbox_api_error)?;
                let queued = self.queue_native_mailbox_relay_wake(&wake, Some(lease))?;
                serde_json::to_value(queued)
            }
        };
        value.map_err(|_| ApiError::internal("native mailbox relay response encoding failed"))
    }

    async fn request_mailbox_relay<T: DeserializeOwned>(
        &self,
        handle: &FederationPeerHandle,
        lease: &FederationLinkLease,
        operation: MailboxRelayOperation,
    ) -> Result<T, ApiError> {
        let request_id = Uuid::new_v4().to_string();
        let expected = lease.with_current(self, |_, peer| Ok(peer.peer.clone()))?;
        let response: MailboxRelayResponse = handle
            .rpc
            .request(FederationRpcRoute::MailboxRelay(
                lease.clone(),
                MailboxRelayRequest {
                    version: MAILBOX_RELAY_VERSION,
                    request_id: request_id.clone(),
                    operation,
                },
            ))
            .await?;
        if response.version != MAILBOX_RELAY_VERSION
            || response.request_id != request_id
            || response.host != expected
        {
            return Err(federation_mailbox_unknown_outcome(
                StatusCode::SERVICE_UNAVAILABLE,
                "native mailbox relay reply identity/version mismatch",
            ));
        }
        lease.with_current(self, |_, _| Ok(())).map_err(|_| {
            federation_mailbox_unknown_outcome(
                StatusCode::SERVICE_UNAVAILABLE,
                "native mailbox relay authority changed after response",
            )
        })?;
        serde_json::from_value(response.value).map_err(|_| {
            federation_mailbox_unknown_outcome(
                StatusCode::SERVICE_UNAVAILABLE,
                "invalid native mailbox relay response",
            )
        })
    }
}
