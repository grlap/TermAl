// Native relay requests over actual mutually attested, MAC-authenticated HTTP.
// No SSH setup, remote process launch or live delivery is claimed by this fixture.
use super::*;

#[path = "relay_recovery.rs"]
mod recovery;

#[path = "relay_review.rs"]
mod review;

#[path = "relay_production_shape.rs"]
mod production_shape;

struct RelayHandleGuard(Vec<AppState>);
impl Drop for RelayHandleGuard {
    fn drop(&mut self) {
        for app in &self.0 {
            app.federation_peer_handles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
        }
    }
}

async fn request(
    rpc: &FederationRpcClient,
    channel: &FederationChannel,
    operation: MailboxRelayOperation,
) -> Result<MailboxRelayResponse, ApiError> {
    let request_id = Uuid::new_v4().to_string();
    let response: MailboxRelayResponse = rpc
        .request(FederationRpcRoute::MailboxRelay(
            channel.current_lease().unwrap(),
            MailboxRelayRequest {
                version: MAILBOX_RELAY_VERSION,
                request_id: request_id.clone(),
                operation,
            },
        ))
        .await?;
    assert_eq!(response.version, MAILBOX_RELAY_VERSION);
    assert_eq!(response.request_id, request_id);
    assert_eq!(response.host, channel.current_authority().unwrap().peer);
    Ok(response)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_rpc_describes_unbound_native_without_changing_its_runtime_or_binding() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let before = fixture.native_snapshot();
    let response = request(
        &fixture.rpc,
        &fixture.channel,
        MailboxRelayOperation::Describe {
            native_session_id: fixture.native.clone(),
        },
    )
    .await
    .unwrap();
    let descriptor: MailboxRelayDescriptor = serde_json::from_value(response.value).unwrap();
    assert_eq!(
        descriptor.principal,
        fixture
            .satellite
            .native_mailbox_relay_descriptor(&fixture.native)
            .unwrap()
            .principal
    );
    assert_eq!(
        descriptor.principal.native_creation_id,
        fixture.native_creation
    );
    assert_eq!(
        before,
        fixture.native_snapshot(),
        "mailbox proof must not install a binding or runtime"
    );
    assert_eq!(
        request(
            &fixture.rpc,
            &fixture.channel,
            MailboxRelayOperation::Describe {
                native_session_id: "session-does-not-exist".into(),
            }
        )
        .await
        .err().expect("missing remote root must refuse").status,
        StatusCode::NOT_FOUND,
        "a typed application NotFound is not a missing relay capability"
    );
    let child = test_session_id(&fixture.satellite, Agent::Codex);
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].session.parent_delegation_id = Some("fixture-delegation".into());
    }
    assert!(
        request(
            &fixture.rpc,
            &fixture.channel,
            MailboxRelayOperation::Describe {
                native_session_id: child
            }
        )
        .await
        .is_err()
    );
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].hidden = true;
    }
    assert!(
        request(
            &fixture.rpc,
            &fixture.channel,
            MailboxRelayOperation::Describe {
                native_session_id: fixture.native.clone()
            }
        )
        .await
        .is_err()
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_rpc_home_send_read_ack_and_opposite_direction_share_one_conversation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let channel = fixture.channel.clone();
    let checked = channel.clone();
    let mut central_server = DomainServer::start(federation_router(
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    ))
    .await;
    let channel = fixture.satellite_channel.clone();
    let checked = channel.clone();
    let reverse = FederationRpcClient::new(
        central_server.port,
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
    .unwrap();
    let sender = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let target = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let home = fixture.central.native_mailbox_relay_home().unwrap();
    let conversation = MailboxRelayConversation::new(
        home.clone(),
        sender.principal.clone(),
        target.principal.clone(),
    )
    .unwrap();
    let append = MailboxRelayAppend {
        requested_target: "session-remote-proxy".into(),
        conversation: conversation.clone(),
        sender: sender.principal.clone(),
        target: target.principal.clone(),
        sender_name: sender.name,
        target_name: target.name,
        message: "durable routine body, not a prompt".into(),
        idempotency_key: "authenticated-send".into(),
        topic: Some("Build status".into()),
        state_stamp: None,
    };
    let response = request(
        &reverse,
        &fixture.satellite_channel,
        MailboxRelayOperation::Send {
            append: append.clone(),
        },
    )
    .await
    .unwrap();
    let receipt: MailboxAppendReceipt = serde_json::from_value(response.value).unwrap();
    assert_eq!(receipt.notification_disposition, "durableButNotWoken");
    let duplicate: MailboxAppendReceipt = serde_json::from_value(
        request(
            &reverse,
            &fixture.satellite_channel,
            MailboxRelayOperation::Send {
                append: append.clone(),
            },
        )
        .await
        .unwrap()
        .value,
    )
    .unwrap();
    assert!(duplicate.duplicate);
    assert_eq!(receipt.message_id, duplicate.message_id);
    let mut reply = append.clone();
    std::mem::swap(&mut reply.sender, &mut reply.target);
    std::mem::swap(&mut reply.sender_name, &mut reply.target_name);
    reply.message = "same native mailbox, reverse direction".into();
    reply.idempotency_key = "authenticated-reply".into();
    let lease = fixture.channel.current_lease().unwrap();
    let replied: MailboxAppendReceipt = serde_json::from_value(
        fixture
            .central
            .execute_mailbox_relay(
                MailboxRelayOperation::Send { append: reply },
                &lease,
                &target.principal.host,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt.mailbox_id, replied.mailbox_id);
    assert_eq!(replied.sequence, 2);
    let page: MailboxReadResponse = serde_json::from_value(
        request(
            &reverse,
            &fixture.satellite_channel,
            MailboxRelayOperation::Read {
                actor: sender.principal.clone(),
                conversation: conversation.clone(),
                mailbox_id: receipt.mailbox_id.clone(),
                after_sequence: None,
                limit: 50,
                issue_receipt: true,
            },
        )
        .await
        .unwrap()
        .value,
    )
    .unwrap();
    assert!(
        page.messages
            .iter()
            .any(|message| message.body == "same native mailbox, reverse direction")
    );
    let summary: MailboxSummary = serde_json::from_value(
        request(
            &reverse,
            &fixture.satellite_channel,
            MailboxRelayOperation::Acknowledge {
                actor: sender.principal.clone(),
                conversation: conversation.clone(),
                mailbox_id: receipt.mailbox_id.clone(),
                receipt: page.receipt.unwrap(),
            },
        )
        .await
        .unwrap()
        .value,
    )
    .unwrap();
    assert_eq!(summary.unread_count, 0);
    assert_eq!(
        fixture
            .satellite
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0,
        "the satellite stores no body replica"
    );
    assert!(
        fixture
            .satellite
            .federation_mailbox_actor(&fixture.native)
            .unwrap()
            .is_none()
    );
    central_server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_rpc_rejects_forged_origin_and_wrong_home_before_any_append() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let central = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let satellite = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    // Central calls the satellite. Claiming the satellite as actor cannot bypass
    // the authenticated source, even though the raw session numbers are equal.
    let home = MailboxRelayHome {
        host: satellite.principal.host.clone(),
        store_id: satellite.principal.store_id.clone(),
    };
    let conversation =
        MailboxRelayConversation::new(home, satellite.principal.clone(), central.principal.clone())
            .unwrap();
    let append = MailboxRelayAppend {
        requested_target: "session-remote-proxy".into(),
        conversation,
        sender: satellite.principal,
        target: central.principal,
        sender_name: satellite.name,
        target_name: central.name,
        message: "must not be appended".into(),
        idempotency_key: "forged-origin".into(),
        topic: None,
        state_stamp: None,
    };
    assert!(
        request(
            &fixture.rpc,
            &fixture.channel,
            MailboxRelayOperation::Send {
                append: append.clone()
            }
        )
        .await
        .is_err()
    );
    let mut wrong_home = append;
    std::mem::swap(&mut wrong_home.sender, &mut wrong_home.target);
    assert!(
        request(
            &fixture.rpc,
            &fixture.channel,
            MailboxRelayOperation::Send { append: wrong_home }
        )
        .await
        .is_err()
    );
    assert_eq!(
        fixture
            .satellite
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_public_remote_proxy_send_reply_read_ack_keeps_local_history_and_metadata_continuation()
 {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let caller = test_session_id(&fixture.central, Agent::Codex);
    let proxy = test_session_id(&fixture.central, Agent::Codex);
    let opposite_proxy = test_session_id(&fixture.satellite, Agent::Codex);
    let old_peer = fixture.native.clone();
    let peer = fixture.channel.current_authority().unwrap();
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        inner.preferences.remotes.push(RemoteConfig {
            id: "native-relay-mac".into(),
            name: "Known Mac".into(),
            transport: RemoteTransport::Ssh,
            enabled: true,
            host: Some(peer.endpoint.ssh_host.clone()),
            port: Some(peer.endpoint.ssh_port),
            user: Some(peer.endpoint.ssh_user.clone()),
        });
        let index = inner.find_session_index(&proxy).unwrap();
        inner.sessions[index].remote_id = Some("native-relay-mac".into());
        inner.sessions[index].remote_session_id = Some(fixture.native.clone());
        let index = inner.find_session_index(&caller).unwrap();
        inner.sessions[index].orchestrator_auto_dispatch_blocked = true;
    }
    let reverse_peer = fixture.satellite_channel.current_authority().unwrap();
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        inner.preferences.remotes.push(RemoteConfig {
            id: "native-relay-central".into(),
            name: "Known central".into(),
            transport: RemoteTransport::Ssh,
            enabled: true,
            host: Some(reverse_peer.endpoint.ssh_host.clone()),
            port: Some(reverse_peer.endpoint.ssh_port),
            user: Some(reverse_peer.endpoint.ssh_user.clone()),
        });
        let proxy_index = inner.find_session_index(&opposite_proxy).unwrap();
        inner.sessions[proxy_index].remote_id = Some("native-relay-central".into());
        inner.sessions[proxy_index].remote_session_id = Some(caller.clone());
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].orchestrator_auto_dispatch_blocked = true;
    }
    let raw = fixture
        .central
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: old_peer.clone(),
            sender_name: "Local peer".into(),
            target_session_id: caller.clone(),
            target_name: "Caller".into(),
            body: "pre-existing local unread body".into(),
            idempotency_key: "local-history".into(),
            topic: None,
            state_stamp: None,
        })
        .unwrap()
        .receipt
        .clone();
    let channel = fixture.channel.clone();
    let checked = channel.clone();
    let mut central_domain = DomainServer::start(federation_router(
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    ))
    .await;
    let channel = fixture.satellite_channel.clone();
    let checked = channel.clone();
    let reverse = FederationRpcClient::new(
        central_domain.port,
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
    .unwrap();
    let _handles = RelayHandleGuard(vec![fixture.central.clone(), fixture.satellite.clone()]);
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            peer.peer.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: fixture.channel.clone(),
                rpc: fixture.rpc.clone(),
            }),
        );
    fixture
        .satellite
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            peer.local.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: fixture.satellite_channel.clone(),
                rpc: reverse,
            }),
        );
    let mut central_api = DomainServer::start(app_router(fixture.central.clone())).await;
    let mut satellite_api = DomainServer::start(app_router(fixture.satellite.clone())).await;
    let central_url = format!(
        "http://127.0.0.1:{}/api/sessions/{caller}",
        central_api.port
    );
    let satellite_url = format!(
        "http://127.0.0.1:{}/api/sessions/{}",
        satellite_api.port, fixture.native
    );
    let body = json!({"targetSessionId":format!("host:{}::{}", peer.peer.host_id, fixture.native),"message":"complete routine body stays only at the central home",
        "idempotencyKey":"public-original","topic":"Status"});
    let response=fixture.client.post(format!("{satellite_url}/mailboxes/send")).json(&json!({
        "targetSessionId":format!("host:{}::{}", peer.local.host_id, caller),"message":"opposite host sends first","idempotencyKey":"opposite-first" })).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let opposite_first: MailboxAppendReceipt = response.json().await.unwrap();
    // The relayed send result's line names the public locator the caller
    // holds, never the home's local id.
    let home_local = MailboxRelayLocator::decode(&opposite_first.message_id, true)
        .unwrap()
        .expect("a relayed receipt carries a public locator")
        .id;
    let note = opposite_first
        .receipt_note
        .as_deref()
        .expect("the relayed send result carries its line");
    assert_eq!(
        note,
        mailbox_send_receipt_note(
            &opposite_first.notification_disposition,
            &opposite_first.message_id,
            false
        )
    );
    assert!(!note.contains(&format!("messageId: \"{home_local}\"")), "{note}");
    // The sender's exact read of it through the relay reports the home's
    // current state as of the read.
    let response = fixture
        .client
        .get(format!(
            "{satellite_url}/mailbox-messages/{}",
            opposite_first.message_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let current: Value = response.json().await.unwrap();
    assert_eq!(current["id"], opposite_first.message_id.as_str());
    assert_eq!(current["currentStatus"]["recipientAcknowledged"], false);
    assert!(
        current["currentStatus"]["summary"]
            .as_str()
            .unwrap()
            .starts_with("As of "),
        "{current}"
    );
    let native_creation = {
        let inner = fixture.satellite.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.native).unwrap()]
            .federation_native_creation_id
            .clone()
    };
    let response = fixture
        .client
        .post(format!("{central_url}/mailboxes/send"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "{}",
        response.text().await.unwrap_or_default()
    );
    // Repeat without consuming the first body above (the assertion only formats on failure).
    let response = fixture
        .client
        .post(format!("{central_url}/mailboxes/send"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let sent: MailboxAppendReceipt = response.json().await.unwrap();
    assert!(sent.duplicate);
    assert_eq!(
        sent.receipt_note.as_deref(),
        Some(
            mailbox_send_receipt_note(&sent.notification_disposition, &sent.message_id, true)
                .as_str()
        )
    );
    assert_eq!(
        sent.mailbox_id, opposite_first.mailbox_id,
        "opposite-first public requests share the immutable central home"
    );
    assert_eq!(sent.sequence, opposite_first.sequence + 1);
    let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false)
        .unwrap()
        .unwrap();
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let (runtime, input_rx) = test_claude_runtime_handle("native-relay-failed-wake");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let native = &mut inner.sessions[index];
        native.session.agent = Agent::Claude;
        native.runtime = SessionRuntime::Claude(runtime);
        native.session.status = SessionStatus::Idle;
        native.orchestrator_auto_dispatch_blocked = false;
    }
    let dispatch = fixture
        .satellite
        .dispatch_next_queued_turn(&fixture.native, false)
        .unwrap()
        .unwrap();
    deliver_turn_dispatch(&fixture.satellite, dispatch).unwrap();
    assert!(matches!(
        recv_within_guard(&input_rx, "native relay metadata handoff"),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));
    let actor = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap()
        .principal;
    assert!(
        fixture
            .satellite
            .mailbox_store
            .mailbox_relay_notification(&actor, &locator.id, sent.sequence)
            .unwrap()
            .unwrap()
            .handed_off
    );
    fixture
        .satellite
        .fail_turn_if_runtime_matches(
            &fixture.native,
            &runtime_token,
            "fixture accepted relay wake failed",
        )
        .unwrap();
    assert!(
        !fixture
            .satellite
            .mailbox_store
            .mailbox_relay_notification(&actor, &locator.id, sent.sequence)
            .unwrap()
            .unwrap()
            .handed_off,
        "failure restores the old handoff boundary without duplicating the home message"
    );
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a failed wake must not spin in its terminal cycle"
    );
    let response = fixture
        .client
        .post(format!(
            "{satellite_url}/mailboxes/{}/read",
            sent.mailbox_id
        ))
        .json(&json!({"issueReceipt":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page: MailboxReadResponse = response.json().await.unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(
        page.messages[0].body,
        "complete routine body stays only at the central home"
    );
    assert_eq!(page.messages[0].id, sent.message_id);
    let reply_to = page.messages[0].sender_session_id.clone();
    assert!(reply_to.starts_with("relay-principal-"));
    let response = fixture
        .client
        .post(format!(
            "{satellite_url}/mailboxes/{}/acknowledge",
            sent.mailbox_id
        ))
        .json(&json!({"receipt":page.receipt,"noContinuation":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = fixture
        .client
        .post(format!(
            "{satellite_url}/mailboxes/{}/acknowledge",
            sent.mailbox_id
        ))
        .json(&json!({"receipt":page.receipt}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let acknowledged: MailboxSummary = response.json().await.unwrap();
    assert_eq!(acknowledged.unread_count, 0);
    let continuation_id = {
        let inner = fixture.satellite.inner.lock().unwrap();
        let native = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        let queued = native
            .queued_prompts
            .iter()
            .find(|queued| {
                queued
                    .pending_prompt
                    .source
                    .as_ref()
                    .and_then(|source| source.mailbox.as_ref())
                    .is_some_and(|mailbox| mailbox.mailbox_id == sent.mailbox_id)
            })
            .unwrap();
        assert!(queued.pending_prompt.text.contains("Nothing is unread"));
        assert!(!queued.pending_prompt.text.contains("complete routine body"));
        assert!(native.federation_mailbox_binding.is_none());
        queued.pending_prompt.id.clone()
    };
    assert_eq!(
        fixture
            .satellite
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    let response=fixture.client.post(format!("{satellite_url}/mailboxes/send")).json(&json!({
        "targetSessionId":reply_to,"message":"reply through the same home","idempotencyKey":"public-reply" })).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let reply: MailboxAppendReceipt = response.json().await.unwrap();
    assert_eq!(reply.mailbox_id, sent.mailbox_id);
    assert_eq!(reply.sequence, sent.sequence + 1);
    let reply_locator = MailboxRelayLocator::decode(&reply.message_id, true).unwrap().unwrap();
    let owner = fixture.central.native_mailbox_relay_descriptor(&caller).unwrap();
    let replied_message = fixture.central.mailbox_store.read_message(&owner.principal.participant_key(), &reply_locator.id).unwrap();
    assert_eq!(replied_message.target_name, owner.name, "replying through a qualified principal retains the freshly proved display name");
    assert_eq!(
        fixture
            .central
            .mailbox_store
            .read_message(&caller, &raw.message_id)
            .unwrap()
            .body,
        "pre-existing local unread body"
    );
    assert_eq!(
        mailbox_processed_through(
            &fixture.central.mailbox_store.connection().unwrap(),
            &raw.mailbox_id,
            &caller
        )
        .unwrap(),
        0
    );
    let response = fixture
        .client
        .get(format!(
            "{central_url}/mailbox-messages/{}",
            reply.message_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let read: MailboxMessage = response.json().await.unwrap();
    assert_eq!(read.body, "reply through the same home");
    let stranger = test_session_id(&fixture.central, Agent::Codex);
    let stranger_url = format!(
        "http://127.0.0.1:{}/api/sessions/{stranger}",
        central_api.port
    );
    assert_eq!(
        fixture
            .client
            .get(format!(
                "{stranger_url}/mailbox-messages/{}",
                sent.message_id
            ))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .client
            .post(format!("{stranger_url}/mailboxes/{}/read", sent.mailbox_id))
            .json(&json!({"issueReceipt":true}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let mut replaced_home = locator.home.clone();
    replaced_home.store_id = Uuid::new_v4().to_string();
    let forged = MailboxRelayLocator::encode(&replaced_home, &locator.id, false).unwrap();
    assert_eq!(
        fixture
            .client
            .post(format!("{central_url}/mailboxes/{forged}/read"))
            .json(&json!({"issueReceipt":true}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let foreign_page = fixture
        .central
        .mailbox_store
        .read_range_with_issuance(&caller, &raw.mailbox_id, None, 50, true)
        .unwrap();
    assert!(
        !fixture
            .client
            .post(format!(
                "{satellite_url}/mailboxes/{}/acknowledge",
                sent.mailbox_id
            ))
            .json(&json!({"receipt":foreign_page.receipt}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let mut changed = body.clone();
    changed["message"] = json!("not the original request");
    assert_eq!(
        fixture
            .client
            .post(format!("{central_url}/mailboxes/send"))
            .json(&changed)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    // Turning the proxy into a local record cannot make an existing relay key
    // silently enter the raw-mailbox namespace.
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        let index = inner.find_session_index(&proxy).unwrap();
        inner.sessions[index].remote_id = None;
        inner.sessions[index].remote_session_id = None;
    }
    let response = fixture
        .client
        .post(format!("{central_url}/mailboxes/send"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let retried: MailboxAppendReceipt = response.json().await.unwrap();
    assert_eq!(retried.message_id, sent.message_id);
    assert_eq!(retried.mailbox_id, sent.mailbox_id);
    fixture
        .satellite
        .cancel_queued_prompt(&fixture.native, &continuation_id)
        .unwrap();
    fixture.satellite.run_mailbox_relay_wake_batch(0, 0).await;
    let wake_json: String = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT wake_json FROM mailbox_relay_wakes WHERE message_id=?1",
            [MailboxRelayLocator::decode(&sent.message_id, true)
                .unwrap()
                .unwrap()
                .id],
            |row| row.get(0),
        )
        .unwrap();
    let wake: MailboxRelayWake = serde_json::from_str(&wake_json).unwrap();
    assert_eq!(
        request(
            &fixture.rpc,
            &fixture.channel,
            MailboxRelayOperation::Wake { wake }
        )
        .await
        .unwrap()
        .value,
        json!(true)
    );
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        let native = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        assert!(
            native.queued_prompts.is_empty(),
            "a duplicate wake or live recovery scan cannot resurrect an explicitly cancelled continuation"
        );
    }
    let mut retargeted = body.clone();
    retargeted["targetSessionId"] = json!(old_peer);
    assert_eq!(
        fixture
            .client
            .post(format!("{central_url}/mailboxes/send"))
            .json(&retargeted)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].federation_native_creation_id = Uuid::new_v4().to_string();
    }
    assert_eq!(
        fixture
            .client
            .post(format!("{central_url}/mailboxes/send"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        fixture
            .central
            .mailbox_store
            .mailbox_relay_association(
                &fixture
                    .central
                    .native_mailbox_relay_descriptor(&caller)
                    .unwrap()
                    .principal,
                &locator.id
            )
            .unwrap()
            .unwrap()
            .home,
        locator.home
    );
    // With the relay link down, the sender's exact read of a message at the
    // remote home says the current status is unavailable and nothing more.
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        inner.sessions[index].federation_native_creation_id = native_creation;
    }
    fixture.satellite_channel.invalidate();
    let response = fixture
        .client
        .get(format!(
            "{satellite_url}/mailbox-messages/{}",
            opposite_first.message_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let unavailable: Value = response.json().await.unwrap();
    let error = unavailable["error"].as_str().expect("an error message");
    assert!(error.starts_with("current status unavailable:"), "{error}");
    assert!(error.contains("says nothing about delivery"), "{error}");
    assert!(!error.contains("reconcile the original operation"), "{error}");
    assert!(unavailable.get("currentStatus").is_none());
    satellite_api.finish().await;
    central_api.finish().await;
    central_domain.finish().await;
    fixture.finish().await;
}
