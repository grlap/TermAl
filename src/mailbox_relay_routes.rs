// Public routine-mail routing into the authenticated, immutable native home.
// Remote proxies are locators only; this facade never invokes a provider or joins a domain.

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct MailboxRelayListing {
    mailboxes: Vec<MailboxSummary>,
    unavailable_relay_homes: Vec<MailboxRelayUnavailableHome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable_central_domain: Option<MailboxRelayUnavailableDomain>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    unavailable_prior_domains: Vec<MailboxUnavailablePriorDomain>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MailboxUnavailablePriorDomain {
    central: FederationHostIdentity,
    participant_id: String,
    reason: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MailboxRelayUnavailableDomain {
    central: FederationHostIdentity,
    availability: &'static str,
    status: u16,
    error: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MailboxRelayUnavailableHome {
    home: MailboxRelayHome,
    availability: &'static str,
    status: u16,
    error: String,
}

fn public_mailbox_relay_summary(
    home: &MailboxRelayHome,
    mut summary: MailboxSummary,
) -> Result<MailboxSummary, ApiError> {
    summary.id = MailboxRelayLocator::encode(home, &summary.id, false)?;
    Ok(summary)
}

fn public_mailbox_relay_message(
    home: &MailboxRelayHome,
    mut message: MailboxMessage,
) -> Result<MailboxMessage, ApiError> {
    message.id = MailboxRelayLocator::encode(home, &message.id, true)?;
    message.mailbox_id = MailboxRelayLocator::encode(home, &message.mailbox_id, false)?;
    Ok(message)
}

fn public_mailbox_relay_receipt(
    home: &MailboxRelayHome,
    mut receipt: MailboxAppendReceipt,
) -> Result<MailboxAppendReceipt, ApiError> {
    receipt.mailbox_id = MailboxRelayLocator::encode(home, &receipt.mailbox_id, false)?;
    receipt.message_id = MailboxRelayLocator::encode(home, &receipt.message_id, true)?;
    Ok(receipt)
}

impl AppState {
    fn mailbox_relay_current_native(&self, actor: &MailboxRelayPrincipal) -> Result<(), ApiError> {
        if self
            .native_mailbox_relay_descriptor(&actor.native_session_id)?
            .principal
            != *actor
        {
            return Err(ApiError::conflict(
                "native mailbox caller was replaced; no fallback",
            ));
        }
        Ok(())
    }

    fn mailbox_relay_peer(
        &self,
        actor: &MailboxRelayPrincipal,
        conversation: &MailboxRelayConversation,
    ) -> Result<
        (
            Arc<FederationPeerHandle>,
            FederationLinkLease,
            FederationPeerSnapshot,
        ),
        ApiError,
    > {
        conversation.require_member(actor)?;
        self.mailbox_relay_current_native(actor)?;
        let other = conversation
            .participants
            .iter()
            .find(|participant| *participant != actor)
            .ok_or_else(|| ApiError::conflict("native mailbox peer missing"))?;
        let peer = self.federation_peer_snapshot(&other.host.host_id)?;
        if peer.local != actor.host
            || peer.peer != other.host
            || peer.central != conversation.home.host
        {
            return Err(ApiError::conflict(
                "native mailbox enrollment/home changed; no fallback",
            ));
        }
        let handle=self.federation_peer_handle(&peer.peer.host_id)
            .ok_or_else(||ApiError::conflict("native mailbox relay requires an enrolled authenticated peer with relay support"))?;
        let lease = handle.channel.current_lease()?;
        lease.with_current(self, |_, current| {
            if !current.same_transport_as(&peer) {
                return Err(federation_stale_link());
            }
            Ok(())
        })?;
        Ok((handle, lease, peer))
    }

    async fn call_native_mailbox_home<T: DeserializeOwned>(
        &self,
        actor: &MailboxRelayPrincipal,
        conversation: &MailboxRelayConversation,
        operation: MailboxRelayOperation,
    ) -> Result<T, ApiError> {
        if conversation.home.host == actor.host
            && matches!(
                &operation,
                MailboxRelayOperation::List { .. }
                    | MailboxRelayOperation::Read { .. }
                    | MailboxRelayOperation::ReadMessage { .. }
                    | MailboxRelayOperation::Acknowledge { .. }
            )
        {
            conversation.require_member(actor)?;
            let app = self.clone();
            let actor = actor.clone();
            let home = conversation.home.clone();
            let value = run_blocking_api(move || {
                app.execute_local_mailbox_relay_history(&actor, &home, operation)
            })
            .await?;
            return serde_json::from_value(value)
                .map_err(|_| ApiError::internal("invalid local native mailbox history reply"));
        }
        let (handle, lease, _) = self.mailbox_relay_peer(actor, conversation)?;
        let value = if conversation.home.host == actor.host {
            let app = self.clone();
            let origin = actor.host.clone();
            let lease = lease.clone();
            let value =
                run_blocking_api(move || app.execute_mailbox_relay(operation, &lease, &origin))
                    .await?;
            serde_json::from_value(value)
                .map_err(|_| ApiError::internal("invalid local native mailbox reply"))?
        } else {
            self.request_mailbox_relay(&handle, &lease, operation)
                .await?
        };
        self.mailbox_relay_current_native(actor)?;
        Ok(value)
    }

    fn mailbox_relay_proxy_locator(
        &self,
        target: &str,
    ) -> Result<Option<(String, FederationPeerSnapshot)>, ApiError> {
        if !target.starts_with("session-") {
            return Ok(None);
        }
        let Some(proxy) = self.remote_session_target(target)? else {
            return Ok(None);
        };
        if !proxy.remote.enabled || proxy.remote.transport != RemoteTransport::Ssh {
            return Err(ApiError::conflict(
                "remote mailbox locator is disabled or unsupported",
            ));
        }
        let peers = self.federation_peer_snapshots()?;
        let mut matches = peers.into_iter().filter(|peer| {
            proxy.remote.host.as_deref() == Some(peer.endpoint.ssh_host.as_str())
                && proxy.remote.port.unwrap_or(22) == peer.endpoint.ssh_port
                && proxy
                    .remote
                    .user
                    .as_deref()
                    .is_none_or(|user| user == peer.endpoint.ssh_user)
        });
        let peer=matches.next().ok_or_else(||ApiError::conflict(
            "remote root has no matching enrolled authenticated host pair for routine mailbox relay"))?;
        if matches.next().is_some() {
            return Err(ApiError::conflict(
                "remote mailbox host locator is ambiguous",
            ));
        }
        Ok(Some((proxy.remote_session_id, peer)))
    }

    async fn send_native_mailbox_relay(
        &self,
        session: &str,
        request: &SendMailboxMessageRequest,
    ) -> Result<Option<MailboxAppendReceipt>, ApiError> {
        let app = self.clone();
        let session_owned = session.to_owned();
        let key = request.idempotency_key.clone();
        let pin = run_blocking_api(move || {
            app.mailbox_store
                .mailbox_relay_intent(&session_owned, &key)
                .map_err(mailbox_api_error)
        })
        .await?;
        let relay_reference = request.target_session_id.starts_with("relay-principal-");
        // Existing pins are resolved before looking at a mutable proxy or taking
        // any legacy route. An unchanged retry always reaches the original home.
        let locator = if pin.is_none() && !relay_reference {
            self.mailbox_relay_proxy_locator(&request.target_session_id)?
        } else {
            None
        };
        if pin.is_none() && !relay_reference && locator.is_none() {
            return Ok(None);
        }
        if request
            .class
            .as_deref()
            .is_some_and(|class| class != "routine")
        {
            return Err(ApiError::bad_request(
                "native mailboxes support only class `routine`",
            ));
        }
        let sender = self.native_mailbox_relay_descriptor(session)?;
        let (conversation, target, pinned_peer) = if let Some((
            conversation,
            peer,
            requested_target,
        )) = pin
        {
            if requested_target != request.target_session_id {
                return Err(ApiError::conflict(
                    "native mailbox idempotency key is pinned to another requested target",
                ));
            }
            conversation.require_member(&sender.principal)?;
            let target = conversation
                .participants
                .iter()
                .find(|actor| **actor != sender.principal)
                .cloned()
                .ok_or_else(|| ApiError::conflict("native mailbox pinned target missing"))?;
            let target = MailboxRelayDescriptor {
                name: target.native_session_id.clone(),
                principal: target,
            };
            (conversation, target, Some(peer))
        } else if relay_reference {
            let app = self.clone();
            let actor = sender.principal.clone();
            let conversations = run_blocking_api(move || {
                app.mailbox_store
                    .mailbox_relay_conversations(&actor)
                    .map_err(mailbox_api_error)
            })
            .await?;
            let conversation = conversations
                .into_iter()
                .find(|conversation| {
                    conversation
                        .participants
                        .iter()
                        .any(|actor| actor.participant_key() == request.target_session_id)
                })
                .ok_or_else(|| {
                    ApiError::not_found("native mailbox reply principal not found for this caller")
                })?;
            let target = conversation
                .participants
                .iter()
                .find(|actor| actor.participant_key() == request.target_session_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found("native mailbox reply principal missing"))?;
            if target == sender.principal {
                return Err(ApiError::bad_request(
                    "native mailbox cannot send to itself",
                ));
            }
            let target = MailboxRelayDescriptor {
                name: target.native_session_id.clone(),
                principal: target,
            };
            (conversation, target, None)
        } else {
            let (native, peer) =
                locator.ok_or_else(|| ApiError::conflict("native mailbox locator missing"))?;
            let handle=self.federation_peer_handle(&peer.peer.host_id).ok_or_else(||ApiError::conflict(
                "native mailbox relay requires the enrolled authenticated peer to be connected"))?;
            let lease = handle.channel.current_lease()?;
            let target: MailboxRelayDescriptor = self
                .request_mailbox_relay(
                    &handle,
                    &lease,
                    MailboxRelayOperation::Describe {
                        native_session_id: native.clone(),
                    },
                )
                .await?;
            target.principal.validate()?;
            if target.principal.host != peer.peer
                || target.principal.native_session_id != native
                || !valid_mailbox_relay_name(&target.name)
            {
                return Err(ApiError::conflict(
                    "authenticated remote mailbox descriptor mismatch",
                ));
            }
            // Re-resolve the locator after the proof; no changed proxy/config may
            // publish a send admitted under an older endpoint snapshot.
            let current = self
                .mailbox_relay_proxy_locator(&request.target_session_id)?
                .ok_or_else(|| ApiError::conflict("remote mailbox locator changed"))?;
            if current.0 != native || !current.1.same_transport_as(&peer) {
                return Err(ApiError::conflict(
                    "remote mailbox locator changed during proof",
                ));
            }
            let home_principal = if peer.central == sender.principal.host {
                &sender.principal
            } else {
                &target.principal
            };
            let home = MailboxRelayHome {
                host: peer.central.clone(),
                store_id: home_principal.store_id.clone(),
            };
            (
                MailboxRelayConversation::new(
                    home,
                    sender.principal.clone(),
                    target.principal.clone(),
                )?,
                target,
                None,
            )
        };
        let (handle, lease, peer) = self.mailbox_relay_peer(&sender.principal, &conversation)?;
        if pinned_peer
            .as_ref()
            .is_some_and(|pinned| !same_mailbox_relay_enrollment(pinned, &peer))
        {
            return Err(ApiError::conflict(
                "native mailbox pinned enrollment changed; reconcile, do not retarget",
            ));
        }
        let fresh: MailboxRelayDescriptor = self
            .request_mailbox_relay(
                &handle,
                &lease,
                MailboxRelayOperation::Describe {
                    native_session_id: target.principal.native_session_id.clone(),
                },
            )
            .await?;
        if fresh.principal != target.principal {
            return Err(ApiError::conflict(
                "native mailbox target was replaced; original request remains pinned",
            ));
        }
        let append = MailboxRelayAppend {
            requested_target: request.target_session_id.clone(),
            conversation: conversation.clone(),
            sender: sender.principal.clone(),
            target: target.principal,
            sender_name: sender.name,
            target_name: fresh.name,
            message: request.message.clone(),
            idempotency_key: request.idempotency_key.clone(),
            topic: request.topic.clone(),
            state_stamp: request.state_stamp.clone(),
        };
        append.input()?;
        let authority = MailboxRelayAuthority {
            app: self.clone(),
            lease: lease.clone(),
            native: vec![sender.principal.clone()],
            home: None,
        };
        let app = self.clone();
        let admitted = append.clone();
        run_blocking_api(move || {
            app.mailbox_store
                .pin_mailbox_relay(&admitted, &peer, || authority.lock())
                .map_err(mailbox_api_error)
        })
        .await?;
        let receipt: MailboxAppendReceipt = self
            .call_native_mailbox_home(
                &sender.principal,
                &conversation,
                MailboxRelayOperation::Send {
                    append: append.clone(),
                },
            )
            .await?;
        let authority = MailboxRelayAuthority {
            app: self.clone(),
            lease,
            native: vec![sender.principal.clone()],
            home: None,
        };
        let app = self.clone();
        let settled = receipt.clone();
        run_blocking_api(move ||app.mailbox_store.settle_mailbox_relay(&append,&settled,||authority.lock()).map_err(mailbox_api_error)).await
            .map_err(|_|federation_mailbox_unknown_outcome(StatusCode::SERVICE_UNAVAILABLE,
                "native mailbox append replied but its local outcome remains unconfirmed; retry identical request/key"))?;
        Ok(Some(public_mailbox_relay_receipt(
            &conversation.home,
            receipt,
        )?))
    }

    async fn list_native_mailbox_relays(
        &self,
        session: &str,
    ) -> Result<MailboxRelayListing, ApiError> {
        let actor = match self.native_mailbox_relay_descriptor(session) {
            Ok(descriptor) => descriptor.principal,
            Err(_) => return Ok(MailboxRelayListing::default()), // standalone roots need no federation identity
        };
        let app = self.clone();
        let captured = actor.clone();
        let conversations = run_blocking_api(move || {
            app.mailbox_store
                .mailbox_relay_conversations(&captured)
                .map_err(mailbox_api_error)
        })
        .await?;
        let mut homes = HashSet::new();
        let mut summaries = Vec::new();
        let mut unavailable_relay_homes = Vec::new();
        for conversation in conversations {
            if !homes
                .insert(serde_json::to_string(&conversation.home).expect("mailbox home serializes"))
            {
                continue;
            }
            let rows: Result<Vec<MailboxRelaySummary>, ApiError> = self
                .call_native_mailbox_home(
                    &actor,
                    &conversation,
                    MailboxRelayOperation::List {
                        actor: actor.clone(),
                        home: conversation.home.clone(),
                    },
                )
                .await;
            let rows = match rows {
                Ok(rows) => rows,
                Err(error) if conversation.home.host != actor.host => {
                    // Absence of a current remote result is neither empty
                    // history nor zero unread mail. Keep that home's error
                    // separate from authoritative local summaries.
                    unavailable_relay_homes.push(MailboxRelayUnavailableHome {
                        home: conversation.home.clone(),
                        availability: "unavailable_unverified",
                        status: error.status.as_u16(),
                        error: error.message,
                    });
                    continue;
                }
                Err(error) => return Err(error), // local corruption/ownership is not peer offline
            };
            for row in rows {
                row.conversation.require_home(&conversation.home)?;
                row.conversation.require_member(&actor)?;
                let names: [String; 2] = row.conversation.participants.clone().map(|principal| {
                    row.summary
                        .participants
                        .iter()
                        .find(|participant| participant.session_id == principal.participant_key())
                        .map(|participant| participant.display_name.clone())
                        .unwrap_or_else(|| principal.native_session_id.clone())
                });
                let app = self.clone();
                let saved = row.conversation.clone();
                let mailbox = row.summary.id.clone();
                run_blocking_api(move || {
                    app.mailbox_store
                        .remember_mailbox_relay(&saved, &mailbox, &names, || Ok(()))
                        .map_err(mailbox_api_error)
                })
                .await?;
                summaries.push(public_mailbox_relay_summary(
                    &conversation.home,
                    row.summary,
                )?);
            }
        }
        Ok(MailboxRelayListing {
            mailboxes: summaries,
            unavailable_relay_homes,
            unavailable_central_domain: None,
            unavailable_prior_domains: Vec::new(),
        })
    }

    async fn read_native_mailbox_relay(
        &self,
        session: &str,
        locator: MailboxRelayLocator,
        request: ReadMailboxRequest,
    ) -> Result<MailboxReadResponse, ApiError> {
        if request.read_key.is_some() {
            return Err(ApiError::bad_request(
                "native relay read uses an issued receipt, not a central-domain readKey",
            ));
        }
        let actor = self.native_mailbox_relay_descriptor(session)?.principal;
        let app = self.clone();
        let captured = actor.clone();
        let mailbox = locator.id.clone();
        let conversation = run_blocking_api(move || {
            app.mailbox_store
                .mailbox_relay_association(&captured, &mailbox)
                .map_err(mailbox_api_error)
        })
        .await?
        .ok_or_else(|| {
            ApiError::not_found(
                "native mailbox association missing; list the original home to recover it",
            )
        })?;
        conversation.require_home(&locator.home)?;
        let mut page: MailboxReadResponse = self
            .call_native_mailbox_home(
                &actor,
                &conversation,
                MailboxRelayOperation::Read {
                    actor: actor.clone(),
                    conversation: conversation.clone(),
                    mailbox_id: locator.id,
                    after_sequence: request.after_sequence,
                    limit: request.limit,
                    issue_receipt: request.issue_receipt,
                },
            )
            .await?;
        page.messages = page
            .messages
            .into_iter()
            .map(|message| public_mailbox_relay_message(&locator.home, message))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(page)
    }

    async fn read_native_mailbox_relay_message(
        &self,
        session: &str,
        locator: MailboxRelayLocator,
    ) -> Result<MailboxMessage, ApiError> {
        let actor = self.native_mailbox_relay_descriptor(session)?.principal;
        let app = self.clone();
        let captured = actor.clone();
        let conversations = run_blocking_api(move || {
            app.mailbox_store
                .mailbox_relay_conversations(&captured)
                .map_err(mailbox_api_error)
        })
        .await?;
        let conversation = conversations
            .into_iter()
            .find(|conversation| conversation.home == locator.home)
            .ok_or_else(|| ApiError::not_found("native mailbox home membership not found"))?;
        let message = self
            .call_native_mailbox_home(
                &actor,
                &conversation,
                MailboxRelayOperation::ReadMessage {
                    actor: actor.clone(),
                    home: locator.home.clone(),
                    message_id: locator.id,
                },
            )
            .await?;
        public_mailbox_relay_message(&locator.home, message)
    }

    async fn acknowledge_native_mailbox_relay(
        &self,
        session: &str,
        locator: MailboxRelayLocator,
        request: AcknowledgeMailboxRequest,
    ) -> Result<MailboxSummary, ApiError> {
        if request.no_continuation {
            return Err(ApiError::bad_request(
                "noContinuation is unavailable for native relay mailboxes; keep the owed continuation",
            ));
        }
        let (Some(receipt), None, None) = (
            request.receipt,
            request.expected_processed_through,
            request.processed_through,
        ) else {
            return Err(ApiError::bad_request(
                "native relay acknowledgement requires only the unchanged issued receipt",
            ));
        };
        let actor = self.native_mailbox_relay_descriptor(session)?.principal;
        let app = self.clone();
        let captured = actor.clone();
        let mailbox = locator.id.clone();
        let conversation = run_blocking_api(move || {
            app.mailbox_store
                .mailbox_relay_association(&captured, &mailbox)
                .map_err(mailbox_api_error)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("native mailbox association not found"))?;
        conversation.require_home(&locator.home)?;
        let summary: MailboxSummary = self
            .call_native_mailbox_home(
                &actor,
                &conversation,
                MailboxRelayOperation::Acknowledge {
                    actor: actor.clone(),
                    conversation: conversation.clone(),
                    mailbox_id: locator.id.clone(),
                    receipt,
                },
            )
            .await?;
        let through = summary
            .participants
            .iter()
            .find(|participant| participant.session_id == actor.participant_key())
            .ok_or_else(|| ApiError::conflict("native mailbox acknowledgement caller missing"))?
            .processed_through;
        let app = self.clone();
        let owned = session.to_owned();
        let refreshed = locator.clone();
        if let Err(error) = run_blocking_api(move || {
            app.refresh_native_mailbox_relay_acknowledgement(&owned, &refreshed, through)
                .map_err(mailbox_api_error)
        })
        .await
        {
            eprintln!(
                "native mailbox acknowledgement committed; wake refresh remains pending: {}",
                error.message
            );
        }
        public_mailbox_relay_summary(&locator.home, summary)
    }
}
