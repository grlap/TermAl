// Authenticated native mailbox operations; principal claims originate on their host.
// Owns no provider/executor entry point and performs no network I/O under state/SQL locks.

use super::*;

pub(crate) const NATIVE_MAILBOX_RELAY_LINK_CHANGED: &str = "native mailbox relay link changed";

pub(crate) enum NativeMailboxRelayDescriptorError {
    StoreRead(ApiError),
    Authority(ApiError),
}

impl NativeMailboxRelayDescriptorError {
    fn into_api_error(self) -> ApiError {
        match self {
            Self::StoreRead(error) | Self::Authority(error) => error,
        }
    }
}

pub(crate) enum MailboxRelayRequestError {
    /// Local route authority refused; nothing was sent to the origin.
    BeforeReadAuthority(ApiError),
    /// The local peer RPC bound or the sender window stayed full; nothing was
    /// sent to the origin.
    BeforeReadCapacity(ApiError),
    /// The read's deadline passed in the local checks before it was sent.
    BeforeReadDeadline(ApiError),
    AfterReadAuthority(ApiError),
    /// The request deadline passed after the read was sent.
    ReadTimeout(ApiError),
    Read(ApiError),
}

impl MailboxRelayRequestError {
    fn into_api_error(self) -> ApiError {
        match self {
            Self::BeforeReadAuthority(error)
            | Self::BeforeReadCapacity(error)
            | Self::BeforeReadDeadline(error)
            | Self::AfterReadAuthority(error)
            | Self::ReadTimeout(error)
            | Self::Read(error) => error,
        }
    }
}

pub(crate) struct MailboxRelayAuthority {
    pub(crate) app: AppState,
    pub(crate) lease: FederationLinkLease,
    pub(crate) native: Vec<MailboxRelayPrincipal>,
    pub(crate) home: Option<MailboxRelayHome>,
}

pub(crate) struct MailboxRelayAdmission<'a> {
    pub(crate) _link: std::sync::MutexGuard<'a, FederationLinkState>,
    new_send_refusal: Option<String>,
}

impl MailboxRelayAdmission<'_> {
    pub(crate) fn authorize_new_send(&self) -> Result<()> {
        match &self.new_send_refusal {
            Some(reason) => Err(relay_store_error(reason)),
            None => Ok(()),
        }
    }
}

impl MailboxRelayAuthority {
    pub(crate) fn lock(&self) -> Result<MailboxRelayAdmission<'_>> {
        self.lock_with(|_| Ok(())).map(|(admission, ())| admission)
    }

    // The callback may inspect in-memory state only. Its result is captured at
    // the same admission cut; the returned token owns no StateInner guard.
    pub(crate) fn lock_with<T>(
        &self,
        inspect: impl FnOnce(&StateInner) -> Result<T>,
    ) -> Result<(MailboxRelayAdmission<'_>, T)> {
        let link = self
            .lease
            .channel
            .state
            .lock()
            .expect("federation link mutex poisoned");
        if link.incarnation.as_deref() != Some(self.lease.incarnation.as_str())
            || link.recorded_incarnation.as_deref() != Some(self.lease.incarnation.as_str())
        {
            return Err(relay_store_error(NATIVE_MAILBOX_RELAY_LINK_CHANGED));
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
        let new_send_refusal = federation_participation_qualified(
            &inner.federation,
            &inner.preferences.federation,
            &peer,
            &self.lease.incarnation,
        )
        .err()
        .map(|error| error.message);
        let captured = inspect(&inner)?;
        // Native relay admission linearizes at this short link -> StateInner
        // cut. An admitted operation may finish with its captured inputs after
        // a state-only change; later operations validate again. Keep only the
        // link fence through COMMIT, never StateInner across store work. New
        // sends consume the captured participation result; exact retries don't.
        drop(inner);
        Ok((
            MailboxRelayAdmission {
                _link: link,
                new_send_refusal,
            },
            captured,
        ))
    }
}

pub(crate) fn mailbox_relay_rpc_routes() -> Router<FederationListenerState> {
    Router::new()
        .route(MAILBOX_RELAY_RPC_ROUTE, post(serve_mailbox_relay))
        .layer(DefaultBodyLimit::max(FEDERATION_MAILBOX_REQUEST_BYTES))
}

pub(crate) async fn serve_mailbox_relay(
    axum::Extension(peer): axum::Extension<FederationAuthenticatedPeer>,
    Json(request): Json<MailboxRelayRequest>,
) -> Result<Response, ApiError> {
    if request.version != MAILBOX_RELAY_VERSION || !mailbox_relay_uuid(&request.request_id) {
        return Err(ApiError::conflict("native mailbox relay upgrade required"));
    }
    let lease = peer.lease.ok_or_else(federation_stale_link)?;
    let app = lease
        .channel
        .app
        .clone()
        .ok_or_else(federation_transport_unavailable)?;
    let permit = peer.mailbox_permit;
    let phases = peer.relay_phases;
    if let Some(phases) = &phases {
        phases.enter("dispatch");
    }
    // Every StateInner wait runs on the blocking job, never on an async
    // worker, so a long state hold cannot stall the runtime or the deadline.
    let is_send = matches!(&request.operation, MailboxRelayOperation::Send { .. });
    let result = run_blocking_api(move || {
        let _permit = permit;
        let _phases = MailboxRelayPhaseScope::install(phases);
        mailbox_relay_phase("origin");
        let origin = lease.with_current(&app, |_, current| Ok(current.peer.clone()))?;
        mailbox_relay_phase("operation");
        let value = app.execute_mailbox_relay(request.operation, &lease, &origin)?;
        let host = lease.with_current(&app, |_, current| Ok(current.local.clone()))?;
        Ok(Json(MailboxRelayResponse {
            version: MAILBOX_RELAY_VERSION,
            request_id: request.request_id,
            host,
            value,
        }))
    })
    .await;
    match result {
        Ok(response) => Ok(response.into_response()),
        Err(error)
            if is_send && error.kind == Some(ApiErrorKind::FederationMailboxWriterAdmission) =>
        {
            let mut response = error.into_response();
            response
                .extensions_mut()
                .insert(MailboxRelayWriterAdmissionRefusal);
            Ok(response)
        }
        Err(error) => Err(error),
    }
}

impl AppState {
    pub(crate) fn native_mailbox_relay_descriptor(
        &self,
        session: &str,
    ) -> Result<MailboxRelayDescriptor, ApiError> {
        self.native_mailbox_relay_descriptor_checked(session)
            .map_err(NativeMailboxRelayDescriptorError::into_api_error)
    }

    pub(crate) fn native_mailbox_relay_descriptor_checked(
        &self,
        session: &str,
    ) -> Result<MailboxRelayDescriptor, NativeMailboxRelayDescriptorError> {
        use NativeMailboxRelayDescriptorError::{Authority, StoreRead};
        let store_error = |error| StoreRead(mailbox_api_error(error));
        mailbox_relay_phase("connection");
        let store_id = {
            let connection = self.mailbox_store.connection().map_err(store_error)?;
            mailbox_relay_phase("storeId");
            mailbox_relay_store_id(&connection).map_err(store_error)?
        };
        mailbox_relay_phase("state");
        let inner = self.inner.lock().expect("state mutex poisoned");
        let root = federation_mailbox_root_locked(&inner, session).map_err(Authority)?;
        let host = inner
            .federation
            .local
            .clone()
            .ok_or_else(|| Authority(federation_transport_unavailable()))?;
        let principal = MailboxRelayPrincipal {
            host,
            store_id,
            native_session_id: session.to_owned(),
            native_creation_id: root.federation_native_creation_id.clone(),
        };
        principal.validate().map_err(Authority)?;
        Ok(MailboxRelayDescriptor {
            principal,
            name: federation_mailbox_display_name(&root.session.name),
        })
    }

    pub(crate) fn native_mailbox_relay_home(&self) -> Result<MailboxRelayHome, ApiError> {
        self.native_mailbox_relay_home_checked(true)
    }

    pub(crate) fn native_mailbox_relay_home_checked(
        &self,
        record_refusal: bool,
    ) -> Result<MailboxRelayHome, ApiError> {
        let store_id = {
            let connection = self.mailbox_store.connection().map_err(mailbox_api_error)?;
            mailbox_relay_store_id(&connection).map_err(mailbox_api_error)?
        };
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(MailboxRelayHome {
            host: federation_mailbox_central_checked(&inner, record_refusal)?,
            store_id,
        })
    }

    pub(crate) fn mailbox_relay_home_authority(
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

    pub(crate) fn require_mailbox_relay_conversation(
        &self,
        actor: &MailboxRelayPrincipal,
        mailbox: &str,
        conversation: &MailboxRelayConversation,
    ) -> Result<(), ApiError> {
        conversation.require_member(actor)?;
        let actual = self
            .mailbox_store
            .mailbox_relay_association(actor, mailbox)
            .map_err(mailbox_relay_locator_lookup_error)?;
        match actual {
            Some(actual) if &actual == conversation => Ok(()),
            Some(_) => Err(ApiError::not_found(
                "native mailbox conversation membership not found",
            )),
            None => Err(self.missing_native_mailbox_relay_association(actor, mailbox)),
        }
    }

    pub(crate) fn execute_mailbox_relay(
        &self,
        operation: MailboxRelayOperation,
        lease: &FederationLinkLease,
        authenticated_origin: &FederationHostIdentity,
    ) -> Result<Value, ApiError> {
        let value = match operation {
            MailboxRelayOperation::CausalTestCompletionReady { hint } => serde_json::to_value(
                self.receive_causal_test_completion_ready(hint, lease, authenticated_origin)?,
            ),
            MailboxRelayOperation::ReadCausalTestCompletion { selector } => serde_json::to_value(
                self.read_causal_test_completion(&selector, lease, authenticated_origin)?,
            ),
            MailboxRelayOperation::ObserveTestRun {
                actor,
                project_scope,
                run_id,
            } => serde_json::to_value(self.read_remote_test_run_origin(
                &actor,
                &project_scope,
                &run_id,
                lease,
                authenticated_origin,
            )?),
            MailboxRelayOperation::Describe { native_session_id } => {
                let descriptor = self.native_mailbox_relay_descriptor(&native_session_id)?;
                let authority = MailboxRelayAuthority {
                    app: self.clone(),
                    lease: lease.clone(),
                    native: vec![descriptor.principal.clone()],
                    home: None,
                };
                mailbox_relay_phase("authority");
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
                    .append_mailbox_relay(
                        &append,
                        &lease.with_current(self, |_, peer| Ok(peer.clone()))?,
                        || authority.lock(),
                        |guard| guard.authorize_new_send(),
                    )
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
            MailboxRelayOperation::LookupMailbox {
                actor,
                home,
                mailbox_id,
            } => {
                actor.require_origin(authenticated_origin)?;
                if !valid_mailbox_relay_id(&mailbox_id) {
                    return Err(ApiError::bad_request("invalid native mailbox id"));
                }
                let authority =
                    self.mailbox_relay_home_authority(lease, &home, std::slice::from_ref(&actor))?;
                if !self
                    .mailbox_store
                    .has_native_relay_mailbox_participant(&actor, &mailbox_id)
                    .map_err(mailbox_api_error)?
                {
                    return Err(unknown_native_mailbox_locator());
                }
                let conversation = self
                    .mailbox_store
                    .mailbox_relay_association(&actor, &mailbox_id)
                    .map_err(mailbox_relay_locator_lookup_error)?
                    .ok_or_else(native_mailbox_home_association_missing)?;
                conversation.require_home(&home)?;
                let _guard = authority.lock().map_err(mailbox_api_error)?;
                serde_json::to_value(())
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
                #[cfg(test)]
                {
                    let before_association = self
                        .mailbox_store
                        .before_relay_read_association
                        .lock()
                        .expect("relay read association observer mutex poisoned")
                        .take();
                    if let Some(before_association) = before_association {
                        before_association();
                    }
                }
                let authority = self.mailbox_relay_home_authority(
                    lease,
                    &conversation.home,
                    std::slice::from_ref(&actor),
                )?;
                self.require_mailbox_relay_conversation(&actor, &mailbox_id, &conversation)?;
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
                    .map_err(mailbox_relay_locator_lookup_error)?;
                page.reader_session_id = Some(actor.participant_key());
                serde_json::to_value(page)
            }
            MailboxRelayOperation::ReadMessage {
                actor,
                home,
                message_id,
            } => {
                actor.require_origin(authenticated_origin)?;
                let authority =
                    self.mailbox_relay_home_authority(lease, &home, std::slice::from_ref(&actor))?;
                let mut message = self
                    .mailbox_store
                    .read_message(&actor.participant_key(), &message_id)
                    .map_err(mailbox_relay_locator_lookup_error)?;
                let conversation = self
                    .mailbox_store
                    .mailbox_relay_association(&actor, &message.mailbox_id)
                    .map_err(mailbox_api_error)?
                    .ok_or_else(|| ApiError::not_found("native mailbox association not found"))?;
                conversation.require_home(&home)?;
                self.mailbox_store
                    .attach_mailbox_relay_home_progress(&mut message)
                    .map_err(mailbox_api_error)?;
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
                let authority = self.mailbox_relay_home_authority(
                    lease,
                    &conversation.home,
                    std::slice::from_ref(&actor),
                )?;
                self.require_mailbox_relay_conversation(&actor, &mailbox_id, &conversation)?;
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
                    .map_err(mailbox_relay_locator_lookup_error)?;
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
            MailboxRelayOperation::InspectWake {
                actor,
                conversation,
                target,
                mailbox_id,
                sequence,
            } => {
                actor.require_origin(authenticated_origin)?;
                serde_json::to_value(self.observe_native_mailbox_relay_target(
                    &actor,
                    &conversation,
                    &target,
                    &mailbox_id,
                    sequence,
                    Some(lease),
                )?)
            }
        };
        value.map_err(|_| ApiError::internal("native mailbox relay response encoding failed"))
    }

    pub(crate) async fn request_mailbox_relay<T: DeserializeOwned>(
        &self,
        handle: &FederationPeerHandle,
        lease: &FederationLinkLease,
        operation: MailboxRelayOperation,
    ) -> Result<T, ApiError> {
        self.request_mailbox_relay_checked(handle, lease, operation, None)
            .await
            .map_err(MailboxRelayRequestError::into_api_error)
    }

    /// The relay request with its failure classified. A `timeout`, counted
    /// from this call, bounds the whole exchange, its local checks and sender
    /// counter allocation included.
    pub(crate) async fn request_mailbox_relay_checked<T: DeserializeOwned>(
        &self,
        handle: &FederationPeerHandle,
        lease: &FederationLinkLease,
        operation: MailboxRelayOperation,
        timeout: Option<Duration>,
    ) -> Result<T, MailboxRelayRequestError> {
        use crate::federation_rpc_authenticated::FederationRpcRequestError;
        use MailboxRelayRequestError::{
            AfterReadAuthority, BeforeReadAuthority, BeforeReadCapacity, BeforeReadDeadline, Read,
            ReadTimeout,
        };
        let deadline = timeout.map(|timeout| {
            tokio::time::Instant::from_std(handle.channel.control.clock.now() + timeout)
        });
        let request_id = Uuid::new_v4().to_string();
        let expected = lease
            .with_current(self, |_, peer| Ok(peer.peer.clone()))
            .map_err(BeforeReadAuthority)?;
        let response: MailboxRelayResponse = handle
            .rpc
            .request_once_within(
                FederationRpcRoute::MailboxRelay(
                    lease.clone(),
                    MailboxRelayRequest {
                        version: MAILBOX_RELAY_VERSION,
                        request_id: request_id.clone(),
                        operation,
                    },
                ),
                deadline,
            )
            .await
            .map_err(|error| match error {
                FederationRpcRequestError::BeforeSendAuthority(error) => BeforeReadAuthority(error),
                FederationRpcRequestError::BeforeSendCapacity(error) => BeforeReadCapacity(error),
                FederationRpcRequestError::BeforeSendDeadline(error) => BeforeReadDeadline(error),
                FederationRpcRequestError::AfterReplyAuthority(error) => AfterReadAuthority(error),
                FederationRpcRequestError::TimedOut(error) => ReadTimeout(error),
                FederationRpcRequestError::Other(error) => Read(error),
            })?;
        if response.version != MAILBOX_RELAY_VERSION
            || response.request_id != request_id
            || response.host != expected
        {
            return Err(Read(federation_mailbox_unknown_outcome(
                StatusCode::SERVICE_UNAVAILABLE,
                "native mailbox relay reply identity/version mismatch",
            )));
        }
        lease.with_current(self, |_, _| Ok(())).map_err(|_| {
            AfterReadAuthority(federation_mailbox_unknown_outcome(
                StatusCode::SERVICE_UNAVAILABLE,
                "native mailbox relay authority changed after response",
            ))
        })?;
        serde_json::from_value(response.value).map_err(|_| {
            Read(federation_mailbox_unknown_outcome(
                StatusCode::SERVICE_UNAVAILABLE,
                "invalid native mailbox relay response",
            ))
        })
    }
}
